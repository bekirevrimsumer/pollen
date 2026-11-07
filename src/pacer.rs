//! Animation frames in step with the display.
//!
//! SetTimer ticks at about 64 Hz with jitter, which looks choppy on a 144 Hz
//! screen. While something animates, this thread waits for the display's next
//! refresh (DWM's vblank timing plus a high-resolution waitable timer) and posts
//! WM_FRAME to the dock, at most one frame in flight. When nothing animates it
//! sleeps, so it costs nothing at rest.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::OnceLock;
use std::thread::Thread;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    CreateWaitableTimerExW, SetWaitableTimer, WaitForSingleObject, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE,
    TIMER_ALL_ACCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the dock once per display refresh while animating.
pub const WM_FRAME: u32 = WM_APP + 3;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicBool = AtomicBool::new(false);
static TARGET: AtomicIsize = AtomicIsize::new(0);
static THREAD: OnceLock<Thread> = OnceLock::new();
static WAKES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SKIPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static WAIT_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Profiling: vblank wake-ups, frames skipped because the last one was still
/// pending, and the average wait per wake-up in ms. Resets the counters.
pub fn take_stats() -> (u32, u32, f32) {
    let wakes = WAKES.swap(0, Ordering::Relaxed);
    let skips = SKIPS.swap(0, Ordering::Relaxed);
    let wait = WAIT_US.swap(0, Ordering::Relaxed) as f32 / 1000.0 / wakes.max(1) as f32;
    (wakes, skips, wait)
}

/// Starts (or keeps) posting frames to `hwnd`.
pub fn start(hwnd: HWND) {
    TARGET.store(hwnd.0 as isize, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Release);
    let thread = THREAD.get_or_init(|| {
        std::thread::Builder::new()
            .name("pollen-frames".into())
            .spawn(run)
            .map(|h| h.thread().clone())
            .expect("frame thread")
    });
    thread.unpark();
}

pub fn stop() {
    ACTIVE.store(false, Ordering::Release);
    PENDING.store(false, Ordering::Release);
}

/// The display refresh period DWM reports, for profiling.
pub fn refresh_ms() -> f32 {
    let mut info = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    let mut frequency = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut frequency);
        let _ = DwmGetCompositionTimingInfo(HWND::default(), &mut info);
    }
    info.qpcRefreshPeriod as f32 * 1000.0 / frequency.max(1) as f32
}

/// The dock has handled the last WM_FRAME; another may be posted.
pub fn frame_done() {
    PENDING.store(false, Ordering::Release);
}

fn run() {
    let timer = unsafe {
        CreateWaitableTimerExW(None, PCWSTR::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0).ok()
    };
    let mut frequency = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    loop {
        if !ACTIVE.load(Ordering::Acquire) {
            std::thread::park();
            continue;
        }
        let before = std::time::Instant::now();
        wait_for_vblank(timer, frequency.max(1));
        WAKES.fetch_add(1, Ordering::Relaxed);
        WAIT_US.fetch_add(before.elapsed().as_micros() as u64, Ordering::Relaxed);
        if PENDING.load(Ordering::Acquire) {
            SKIPS.fetch_add(1, Ordering::Relaxed);
        }
        if ACTIVE.load(Ordering::Acquire) && !PENDING.swap(true, Ordering::AcqRel) {
            let hwnd = HWND(TARGET.load(Ordering::Relaxed) as *mut _);
            unsafe {
                if PostMessageW(Some(hwnd), WM_FRAME, WPARAM(0), LPARAM(0)).is_err() {
                    PENDING.store(false, Ordering::Release);
                }
            }
        }
    }
}

/// Sleeps until just after the display's next refresh.
fn wait_for_vblank(timer: Option<HANDLE>, frequency: i64) {
    let mut now = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut now);
    }
    let mut info = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    let timing = unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }.is_ok();
    let period = info.qpcRefreshPeriod as i64;
    let wait = if timing && period > 0 {
        // DWM reports the last vblank while idle but the upcoming one while it is
        // composing; either way the target is the first vblank after now.
        let mut next = info.qpcVBlank as i64;
        if next <= now {
            next += ((now - next) / period + 1) * period;
        }
        next - now
    } else {
        frequency / 144
    };
    // Waitable timers take 100 ns units; negative means relative.
    let due = -(wait * 10_000_000 / frequency).max(1);
    match timer {
        Some(timer) => unsafe {
            if SetWaitableTimer(timer, &due, 0, None, None, false).is_ok() {
                WaitForSingleObject(timer, INFINITE);
            } else {
                std::thread::sleep(std::time::Duration::from_micros((-due / 10) as u64));
            }
        },
        None => std::thread::sleep(std::time::Duration::from_micros((-due / 10) as u64)),
    }
}
