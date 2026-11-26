//! Servidor IPC: una ventana oculta con clase propia que responde consultas
//! QUERY2W. La ventana es de nivel superior (no *message-only*) a propósito:
//! `FindWindowW` no encuentra ventanas message-only.

use std::sync::{Arc, Mutex, OnceLock};

use crate::protocol::{self, ReplyItem, Request, CAPS, IS_DB_LOADED};

/// Clase propia. Es la que busca el cliente de CensusIndex y el shim.
pub const CLASS: &str = "CENSUS_IPC";
/// Clase de Everything: sólo se registra en modo compatibilidad y si está libre.
pub const COMPAT_CLASS: &str = "EVERYTHING_TASKBAR_NOTIFICATION";

/// Lo único que el motor tiene que saber del IPC.
pub trait Searcher: Send + Sync + 'static {
    fn is_ready(&self) -> bool;
    /// Devuelve (total de coincidencias, página pedida).
    fn search(
        &self,
        search: &str,
        sort: u32,
        offset: u32,
        max: u32,
        flags: u32,
    ) -> (u32, Vec<ReplyItem>);
    /// Campos que este motor completa de verdad (ver `protocol::caps`).
    /// Por defecto, todos menos el historial de ejecución.
    fn caps(&self) -> u32 {
        protocol::caps::DEFAULT
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::slice;

    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
    };
    use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
        DispatchMessageW, GetMessageW, PostQuitMessage, RegisterClassW, SendMessageW,
        TranslateMessage, MSG, MSGFLT_ALLOW, WM_COPYDATA, WM_DESTROY, WM_USER, WNDCLASSW, WS_POPUP,
    };

    /// Mensajes que un cliente de integridad media puede mandarle a un
    /// `censusindex serve` elevado. Si falta uno, UIPI lo bloquea con error 5 y la
    /// app cree que CensusIndex no está corriendo.
    pub const ALLOWED_FROM_LOWER_INTEGRITY: [u32; 3] =
        [WM_COPYDATA, WM_USER + IS_DB_LOADED, WM_USER + CAPS];

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

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_COPYDATA => {
                if lparam == 0 {
                    return 0;
                }
                let cds = unsafe { &*(lparam as *const COPYDATASTRUCT) };
                if cds.dwData != protocol::QUERY2W || cds.lpData.is_null() {
                    return 0;
                }
                let raw =
                    unsafe { slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
                let Some(req) = protocol::parse_request(raw) else {
                    return 0;
                };
                let reply = with_searcher(|s| {
                    let (total, items) =
                        s.search(&req.search, req.sort, req.offset, req.max, req.flags);
                    // El `flags` de la respuesta dice qué campos vinieron de
                    // verdad: si el cliente pidió `run_count` y no lo tenemos,
                    // no se anuncia y no se escribe. Un cero no es un dato.
                    let flags = protocol::caps::present(req.flags) & s.caps();
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
            m if m == WM_USER + IS_DB_LOADED => {
                with_searcher(|s| s.is_ready() as LRESULT).unwrap_or(0)
            }
            // Capacidades del motor: qué campos completa y si los metadatos son
            // parciales. 0 = todavía no hay índice.
            m if m == WM_USER + CAPS => with_searcher(|s| s.caps() as LRESULT).unwrap_or(0),
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
        // Una ventana por clase pedida: `--compat` tiene que dejar disponibles
        // CENSUS_IPC *y* la clase de Everything a la vez, no una u otra.
        let mut hwnds: Vec<(String, HWND)> = Vec::new();
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
                    last_err = format!("la clase {name} ya está registrada en este proceso");
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
                    WS_POPUP,
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
            // poder consultar un `censusindex serve` elevado. Sin esto, UIPI bloquea
            // WM_COPYDATA y también WM_USER+IS_DB_LOADED, y el cliente concluye
            // "CensusIndex no está en ejecución" aunque el proceso esté vivo.
            //
            // Estos tres filtros no se sacan: sin ellos la integración con las
            // apps (que corren sin elevación) deja de funcionar. Lo cubre
            // `messages_a_medium_client_needs_are_filtered`.
            unsafe {
                for msg in ALLOWED_FROM_LOWER_INTEGRITY {
                    ChangeWindowMessageFilterEx(hwnd, msg, MSGFLT_ALLOW, std::ptr::null_mut());
                }
            }
            hwnds.push(((*name).to_string(), hwnd));
        }

        if hwnds.is_empty() {
            return Err(last_err);
        }

        // Un solo bucle de mensajes atiende todas las ventanas.
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
        for (_, hwnd) in &hwnds {
            unsafe { DestroyWindow(*hwnd) };
        }
        Ok(hwnds
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
            .join(", "))
    }
}

#[cfg(windows)]
pub use win::{serve, wide, ALLOWED_FROM_LOWER_INTEGRITY};

#[cfg(not(windows))]
pub fn serve(_searcher: Arc<dyn Searcher>, _classes: &[&str]) -> Result<String, String> {
    Err("IPC de CensusIndex sólo existe en Windows".into())
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
