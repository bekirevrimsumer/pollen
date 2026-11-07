//! HTTPS GETs with WinHTTP's defaults: the system proxy and normal
//! certificate validation. Used by the weather and the update check.

use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE,
};

use crate::util::{pcwstr, wide};

/// The body of an HTTPS GET, up to `limit` bytes. WinHTTP follows redirects
/// (GitHub's downloads redirect to its file host).
pub fn get(url: &str, limit: usize) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".to_string()));
    let host_w = wide(host);
    let path_w = wide(&path);
    unsafe {
        // The user agent names the program; GitHub's API requires one.
        let session = WinHttpOpen(w!("Pollen"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return None;
        }
        let connect = WinHttpConnect(session, pcwstr(&host_w), INTERNET_DEFAULT_HTTPS_PORT, 0);
        let request = if connect.is_null() {
            std::ptr::null_mut()
        } else {
            WinHttpOpenRequest(connect, w!("GET"), pcwstr(&path_w), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE)
        };
        let mut body = Vec::new();
        let ok = !request.is_null()
            && WinHttpSendRequest(request, None, None, 0, 0, 0).is_ok()
            && WinHttpReceiveResponse(request, std::ptr::null_mut()).is_ok();
        let mut complete = ok;
        if ok {
            loop {
                let mut available = 0u32;
                if WinHttpQueryDataAvailable(request, &mut available).is_err() {
                    complete = false;
                    break;
                }
                if available == 0 {
                    break;
                }
                let mut chunk = vec![0u8; available as usize];
                let mut read = 0u32;
                if WinHttpReadData(request, chunk.as_mut_ptr() as *mut _, available, &mut read).is_err() || read == 0 {
                    complete = false;
                    break;
                }
                body.extend_from_slice(&chunk[..read as usize]);
                if body.len() > limit {
                    complete = false;
                    break;
                }
            }
        }
        for handle in [request, connect, session] {
            if !handle.is_null() {
                let _ = WinHttpCloseHandle(handle);
            }
        }
        complete.then_some(body)
    }
}

pub fn get_json(url: &str) -> Option<serde_json::Value> {
    serde_json::from_slice(&get(url, 1_000_000)?).ok()
}
