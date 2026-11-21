//! Cliente IPC: manda QUERY2W y espera la respuesta por `WM_COPYDATA`.

use crate::protocol::{self, Request};
use crate::server::{CLASS, COMPAT_CLASS};

#[cfg(windows)]
mod win {
    use super::*;
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowW, PeekMessageW, RegisterClassW,
        SendMessageW, TranslateMessage, HWND_MESSAGE, MSG, PM_REMOVE, WM_COPYDATA, WM_USER, WNDCLASSW,
    };

    use crate::protocol::IS_DB_LOADED;

    const REPLY_CLASS: &str = "CensusIpcReply";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Orden de búsqueda: primero Census, después el Everything real.
    pub fn find_server() -> HWND {
        for cls in [CLASS, COMPAT_CLASS] {
            let c = wide(cls);
            let h = unsafe { FindWindowW(c.as_ptr(), std::ptr::null()) };
            if !h.is_null() {
                return h;
            }
        }
        std::ptr::null_mut()
    }

    thread_local! {
        static REPLY: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn reply_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_COPYDATA && lparam != 0 {
            let cds = unsafe { &*(lparam as *const COPYDATASTRUCT) };
            if !cds.lpData.is_null() {
                let data = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
                REPLY.with(|r| *r.borrow_mut() = Some(data.to_vec()));
                return 1;
            }
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    pub struct Client {
        reply: HWND,
        counter: u32,
    }

    impl Client {
        pub fn new() -> Result<Self, String> {
            unsafe {
                let hinst = GetModuleHandleW(std::ptr::null());
                let cls = wide(REPLY_CLASS);
                let wc = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: Some(reply_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: hinst,
                    hIcon: std::ptr::null_mut(),
                    hCursor: std::ptr::null_mut(),
                    hbrBackground: std::ptr::null_mut(),
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: cls.as_ptr(),
                };
                RegisterClassW(&wc); // si ya existe, alcanza
                let reply = CreateWindowExW(
                    0,
                    cls.as_ptr(),
                    cls.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    hinst,
                    std::ptr::null_mut(),
                );
                if reply.is_null() {
                    return Err("no se pudo crear la ventana de respuesta".into());
                }
                Ok(Self { reply, counter: 1 })
            }
        }

        pub fn is_server_running(&self) -> bool {
            !find_server().is_null()
        }

        /// Sólo mira la clase propia: si esto es false, lo que haya es el
        /// Everything real, no un demonio de Census.
        pub fn census_is_running(&self) -> bool {
            let c = wide(CLASS);
            !unsafe { FindWindowW(c.as_ptr(), std::ptr::null()) }.is_null()
        }

        pub fn is_ready(&self) -> bool {
            let hwnd = find_server();
            if hwnd.is_null() {
                return false;
            }
            let r = unsafe { SendMessageW(hwnd, WM_USER + IS_DB_LOADED, 0, 0) };
            r != 0
        }

        /// Manda la consulta y devuelve la respuesta cruda (bytes del `WM_COPYDATA`).
        pub fn query_raw(&mut self, search: &str, sort: u32, offset: u32, max: u32, flags: u32) -> Option<Vec<u8>> {
            let target = find_server();
            if target.is_null() {
                return None;
            }
            self.counter = self.counter.wrapping_add(1);
            let reply_msg = self.counter;
            let req = Request {
                reply_hwnd: self.reply as usize as u32,
                reply_msg,
                offset,
                max,
                flags,
                sort,
                search: search.to_string(),
            };
            let mut buf = protocol::build_request(&req);
            let cds = COPYDATASTRUCT {
                dwData: protocol::QUERY2W,
                cbData: buf.len() as u32,
                lpData: buf.as_mut_ptr() as *mut c_void,
            };
            REPLY.with(|r| *r.borrow_mut() = None);
            let ok = unsafe { SendMessageW(target, WM_COPYDATA, self.reply as usize, &cds as *const _ as isize) };
            if ok == 0 {
                return None;
            }
            // El servidor puede responder de forma sincrónica (ya está) o
            // diferida, como Everything: en ese caso bombeamos mensajes.
            let start = Instant::now();
            loop {
                if let Some(data) = REPLY.with(|r| r.borrow_mut().take()) {
                    return Some(data);
                }
                unsafe {
                    let mut msg: MSG = std::mem::zeroed();
                    while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                if start.elapsed() > Duration::from_secs(120) {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        /// Consulta completa paginando hasta agotar `total`.
        pub fn query_all(&mut self, search: &str, sort: u32, page: u32) -> Result<protocol::Reply, String> {
            let mut out = protocol::Reply::default();
            let mut offset = 0u32;
            let mut first = true;
            loop {
                let raw = self
                    .query_raw(search, sort, offset, page, protocol::req::BASIC)
                    .ok_or_else(|| "el servidor no respondió".to_string())?;
                let reply = protocol::parse_reply(&raw).ok_or_else(|| "respuesta IPC inválida".to_string())?;
                if first {
                    out.total = reply.total;
                    out.flags = reply.flags;
                    first = false;
                }
                let n = reply.items.len() as u32;
                out.items.extend(reply.items);
                offset += n;
                if n == 0 || offset >= out.total {
                    return Ok(out);
                }
            }
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.reply) };
        }
    }
}

#[cfg(windows)]
pub use win::{find_server, Client};

#[cfg(not(windows))]
pub struct Client;

#[cfg(not(windows))]
impl Client {
    pub fn new() -> Result<Self, String> {
        Err("IPC de Census sólo existe en Windows".into())
    }
}
