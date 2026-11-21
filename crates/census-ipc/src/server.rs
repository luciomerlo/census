//! Servidor IPC: una ventana oculta con clase propia que responde consultas
//! QUERY2W. La ventana es de nivel superior (no *message-only*) a propósito:
//! `FindWindowW` no encuentra ventanas message-only.

use std::sync::{Arc, Mutex, OnceLock};

use crate::protocol::{self, ReplyItem, Request, IS_DB_LOADED};

/// Clase propia. Es la que busca el cliente de Census y el shim.
pub const CLASS: &str = "CENSUS_IPC";
/// Clase de Everything: sólo se registra en modo compatibilidad y si está libre.
pub const COMPAT_CLASS: &str = "EVERYTHING_TASKBAR_NOTIFICATION";

/// Lo único que el motor tiene que saber del IPC.
pub trait Searcher: Send + Sync + 'static {
    fn is_ready(&self) -> bool;
    /// Devuelve (total de coincidencias, página pedida).
    fn search(&self, search: &str, sort: u32, offset: u32, max: u32, flags: u32) -> (u32, Vec<ReplyItem>);
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::slice;

    use windows_sys::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetMessageW, MSGFLT_ALLOW, PostQuitMessage, RegisterClassW, SendMessageW, TranslateMessage, MSG, WM_COPYDATA,
        WM_DESTROY, WM_USER, WNDCLASSW,
    };

    static SEARCHER: OnceLock<Mutex<Option<Arc<dyn Searcher>>>> = OnceLock::new();

    fn slot() -> &'static Mutex<Option<Arc<dyn Searcher>>> {
        SEARCHER.get_or_init(|| Mutex::new(None))
    }

    fn with_searcher<R>(f: impl FnOnce(&Arc<dyn Searcher>) -> R) -> Option<R> {
        let guard = slot().lock().ok()?;
        guard.as_ref().map(f)
    }

    pub fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_COPYDATA => {
                if lparam == 0 {
                    return 0;
                }
                let cds = unsafe { &*(lparam as *const COPYDATASTRUCT) };
                if cds.dwData != protocol::QUERY2W || cds.lpData.is_null() {
                    return 0;
                }
                let raw = unsafe { slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
                let Some(req) = protocol::parse_request(raw) else {
                    return 0;
                };
                let reply = with_searcher(|s| {
                    let (total, items) = s.search(&req.search, req.sort, req.offset, req.max, req.flags);
                    // Sólo se devuelven los campos que el motor sabe completar.
                    let flags = req.flags
                        & (protocol::req::NAME
                            | protocol::req::PATH
                            | protocol::req::FULL_PATH
                            | protocol::req::SIZE
                            | protocol::req::DATE_MODIFIED
                            | protocol::req::DATE_ACCESSED
                            | protocol::req::ATTRIBUTES
                            | protocol::req::RUN_COUNT);
                    protocol::build_reply(total, req.offset, flags, &items)
                });
                let Some(data) = reply else {
                    return 0;
                };
                if req.reply_hwnd == 0 {
                    return 1;
                }
                let cds_reply = COPYDATASTRUCT {
                    dwData: req.reply_msg as usize,
                    cbData: data.len() as u32,
                    lpData: data.as_ptr() as *mut c_void,
                };
                unsafe {
                    SendMessageW(
                        req.reply_hwnd as HWND,
                        WM_COPYDATA,
                        0,
                        &cds_reply as *const COPYDATASTRUCT as isize,
                    );
                }
                1
            }
            m if m == WM_USER + IS_DB_LOADED => with_searcher(|s| s.is_ready() as LRESULT).unwrap_or(0),
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// Registra la primera clase disponible y corre el bucle de mensajes.
    pub fn serve(searcher: Arc<dyn Searcher>, classes: &[&str]) -> Result<String, String> {
        *slot().lock().map_err(|_| "searcher envenenado")? = Some(searcher);
        let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
        if hinst.is_null() {
            return Err("GetModuleHandleW falló".into());
        }

        let mut last_err = String::from("sin clases para registrar");
        for name in classes {
            let cls = wide(name);
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: cls.as_ptr(),
            };
            if unsafe { RegisterClassW(&wc) } == 0 {
                let err = unsafe { GetLastError() } as i32;
                if err == ERROR_CLASS_ALREADY_EXISTS as i32 {
                    last_err = format!("la clase {name} ya está registrada por otro proceso");
                    continue;
                }
                last_err = format!("RegisterClassW({name}) falló: {err}");
                continue;
            }

            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    cls.as_ptr(),
                    cls.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    hinst,
                    std::ptr::null_mut(),
                )
            };
            if hwnd.is_null() {
                last_err = format!("CreateWindowExW({name}) falló");
                continue;
            }

            // Un cliente de integridad media (WizEverything/SortEverything) debe
            // poder consultar un `census serve` elevado. Sin esto, UIPI bloquea
            // WM_COPYDATA y también WM_USER+IS_DB_LOADED, y el cliente concluye
            // "Census no está en ejecución" aunque el proceso esté vivo.
            unsafe {
                ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, std::ptr::null_mut());
                ChangeWindowMessageFilterEx(hwnd, WM_USER + IS_DB_LOADED, MSGFLT_ALLOW, std::ptr::null_mut());
            }

            let mut msg: MSG = unsafe { std::mem::zeroed() };
            loop {
                let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
                if r == 0 || r == -1 {
                    break;
                }
                unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            unsafe { DestroyWindow(hwnd) };
            return Ok((*name).to_string());
        }
        Err(last_err)
    }
}

#[cfg(windows)]
pub use win::{serve, wide};

#[cfg(not(windows))]
pub fn serve(_searcher: Arc<dyn Searcher>, _classes: &[&str]) -> Result<String, String> {
    Err("IPC de Census sólo existe en Windows".into())
}

/// Pedido ya validado, útil para el shim y para tests.
pub fn request_for(search: &str, sort: u32, offset: u32, max: u32, flags: u32) -> Request {
    Request {
        reply_hwnd: 0,
        reply_msg: 0,
        offset,
        max,
        flags,
        sort,
        search: search.to_string(),
    }
}
