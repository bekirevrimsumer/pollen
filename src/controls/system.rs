//! CPU, memory, disk and network use, for the notch's Status tab.
//!
//! Each reading is a cheap system call; the CPU and network figures are rates,
//! so a `Sampler` keeps the previous counters and gives the change since then.

use std::time::Instant;

use windows::core::w;
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IF_OPER_STATUS;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::Win32::Foundation::FILETIME;

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// 0..=1
    pub cpu: f32,
    pub memory_used: u64,
    pub memory_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
    /// Bytes per second.
    pub down: f64,
    pub up: f64,
}

pub struct Sampler {
    cpu: Option<(u64, u64)>,
    net: Option<(u64, u64, Instant)>,
    pub last: Stats,
}

fn ticks(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

impl Sampler {
    pub fn new() -> Self {
        let mut s = Sampler { cpu: None, net: None, last: Stats::default() };
        s.sample();
        s
    }

    /// Reads everything; rates are since the previous call.
    pub fn sample(&mut self) -> Stats {
        let mut stats = self.last;

        // CPU: busy share of the time that passed, from the system-wide times.
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        if unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.is_ok() {
            let (idle, total) = (ticks(idle), ticks(kernel) + ticks(user));
            if let Some((last_idle, last_total)) = self.cpu {
                let (di, dt) = (idle.saturating_sub(last_idle), total.saturating_sub(last_total));
                if dt > 0 {
                    stats.cpu = (1.0 - di as f32 / dt as f32).clamp(0.0, 1.0);
                }
            }
            self.cpu = Some((idle, total));
        }

        let mut memory = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut memory) }.is_ok() {
            stats.memory_total = memory.ullTotalPhys;
            stats.memory_used = memory.ullTotalPhys - memory.ullAvailPhys;
        }

        let (mut free, mut total) = (0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(w!("C:\\"), None, Some(&mut total), Some(&mut free)) }.is_ok() {
            stats.disk_total = total;
            stats.disk_used = total.saturating_sub(free);
        }

        if let Some((down, up)) = network_octets() {
            let now = Instant::now();
            if let Some((last_down, last_up, at)) = self.net {
                let secs = (now - at).as_secs_f64();
                if secs > 0.05 {
                    stats.down = down.saturating_sub(last_down) as f64 / secs;
                    stats.up = up.saturating_sub(last_up) as f64 / secs;
                }
            }
            self.net = Some((down, up, now));
        }

        self.last = stats;
        stats
    }
}

/// Bytes received and sent so far on the physical interfaces that are up.
fn network_octets() -> Option<(u64, u64)> {
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() {
            return None;
        }
        let count = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
        let (mut down, mut up) = (0u64, 0u64);
        for row in rows {
            // Up (1), hardware interfaces only: virtual adapters (VPNs, Hyper-V)
            // would count the same traffic twice.
            let hardware = row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
            if row.OperStatus == IF_OPER_STATUS(1) && hardware {
                down += row.InOctets;
                up += row.OutOctets;
            }
        }
        FreeMibTable(table as *const _);
        Some((down, up))
    }
}

/// How long Windows has been running: "3 sa 12 dk", "2 gün 4 sa".
pub fn uptime() -> String {
    let minutes = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() } / 60_000;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days} gün {hours} sa")
    } else if hours > 0 {
        format!("{hours} sa {minutes} dk")
    } else {
        format!("{minutes} dk")
    }
}

/// Pollen's own memory, as Task Manager shows it (private working set).
pub fn own_memory() -> Option<u64> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX2 { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32, ..Default::default() };
    unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS, counters.cb).ok()?;
    }
    Some(counters.PrivateWorkingSetSize as u64)
}

/// "9,7 GB", "640 MB".
pub fn format_bytes(bytes: u64) -> String {
    let gb = bytes as f64 / 1024f64.powi(3);
    if gb >= 1.0 {
        format!("{:.1} GB", gb).replace('.', ",")
    } else {
        format!("{:.0} MB", bytes as f64 / 1024f64.powi(2))
    }
}

/// "1,2 MB/s", "40 KB/s".
pub fn format_rate(bytes_per_second: f64) -> String {
    let kb = bytes_per_second / 1024.0;
    if kb >= 1024.0 {
        format!("{:.1} MB/s", kb / 1024.0).replace('.', ",")
    } else {
        format!("{:.0} KB/s", kb)
    }
}
