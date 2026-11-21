//! Acceso de bajo nivel al volumen NTFS: handle, geometría y lectura posicional.

#![cfg(windows)]

use std::ffi::c_void;
use std::io;

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE, LUID,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, SetFilePointerEx, FILE_BEGIN, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::IO::DeviceIoControl;

pub const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x0009_0064;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Geometría cruda del volumen. Espeja `NTFS_VOLUME_DATA_BUFFER`.
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct NtfsVolumeData {
    pub volume_serial_number: i64,
    pub number_sectors: i64,
    pub total_clusters: i64,
    pub free_clusters: i64,
    pub total_reserved: i64,
    pub bytes_per_sector: u32,
    pub bytes_per_cluster: u32,
    pub bytes_per_file_record_segment: u32,
    pub clusters_per_file_record_segment: u32,
    pub mft_valid_data_length: i64,
    pub mft_start_lcn: i64,
    pub mft2_start_lcn: i64,
    pub mft_zone_start: i64,
    pub mft_zone_end: i64,
}

pub struct VolumeHandle(HANDLE);

// El handle es un valor del kernel: se puede compartir entre hilos.
unsafe impl Send for VolumeHandle {}
unsafe impl Sync for VolumeHandle {}

impl Drop for VolumeHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl VolumeHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }

    /// Lee exactamente `buf.len()` bytes en `offset` (posición absoluta del volumen).
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut pos = offset as i64;
        let ok = unsafe { SetFilePointerEx(self.0, pos, &mut pos, FILE_BEGIN) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut done = 0usize;
        while done < buf.len() {
            let mut read: u32 = 0;
            let want = (buf.len() - done).min(u32::MAX as usize) as u32;
            let ok = unsafe {
                ReadFile(
                    self.0,
                    buf[done..].as_mut_ptr(),
                    want,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "lectura corta del volumen",
                ));
            }
            done += read as usize;
        }
        Ok(())
    }
}

/// Abre `\\.\X:` para lectura. Requiere elevación.
pub fn open_volume(letter: char) -> io::Result<VolumeHandle> {
    let path = wide(&format!("\\\\.\\{}:", letter.to_ascii_uppercase()));
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(VolumeHandle(h))
}

pub fn volume_data(h: &VolumeHandle) -> io::Result<NtfsVolumeData> {
    let mut out = NtfsVolumeData::default();
    let mut returned: u32 = 0;
    let size = std::mem::size_of::<NtfsVolumeData>() as u32;
    let ok = unsafe {
        DeviceIoControl(
            h.raw(),
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            &mut out as *mut NtfsVolumeData as *mut c_void,
            size,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if out.bytes_per_cluster == 0 || out.bytes_per_file_record_segment == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "geometría NTFS inválida",
        ));
    }
    Ok(out)
}

/// Habilita un privilegio en el token actual. Devuelve si quedó habilitado.
pub fn enable_privilege(name: &str) -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return false;
        }
        let wname = wide(name);
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        if LookupPrivilegeValueW(std::ptr::null(), wname.as_ptr(), &mut luid) == 0 {
            CloseHandle(token);
            return false;
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let ok =
            AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
        let last = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        CloseHandle(token);
        ok != 0 && last == 0
    }
}

/// Area de trabajo (pantalla menos barra de tareas), en píxeles. Es lo que hay
/// que respetar para que la ventana no quede tapada por la barra.
pub fn work_area() -> (i32, i32) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut RECT as *mut c_void, 0) };
    let w = r.right - r.left;
    let h = r.bottom - r.top;
    if ok != 0 && w > 0 && h > 0 {
        (w, h)
    } else {
        (1280, 720)
    }
}

/// Esconde la consola que Windows le da a un binario de subsistema consola.
/// Solo se llama al abrir la ventana: el CLI la sigue necesitando.
pub fn hide_console() {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    unsafe {
        let h = GetConsoleWindow();
        if !h.is_null() {
            ShowWindow(h, SW_HIDE);
        }
    }
}

/// Lo minimo para leer `$MFT` y abrir el volumen.
pub fn enable_raw_volume_access() -> (bool, bool) {
    let backup = enable_privilege("SeBackupPrivilege");
    let manage = enable_privilege("SeManageVolumePrivilege");
    (backup, manage)
}

/// ¿El proceso corre con token elevado? (lectura de la MFT.)
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elev = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut devuelto = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elev as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut devuelto,
        );
        CloseHandle(token);
        ok != 0 && elev.TokenIsElevated != 0
    }
}

/// Relanza este ejecutable elevado (UAC) conservando los argumentos.
/// Devuelve `true` si el nuevo proceso arrancó.
pub fn relaunch_elevated(args: &[String]) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe: Vec<u16> = exe
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let verbo: Vec<u16> = "runas ".encode_utf16().collect();
    let params = args.join(" ");
    let params: Vec<u16> = params.encode_utf16().chain(std::iter::once(0)).collect();
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verbo.as_ptr(),
            exe.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW devuelve un HINSTANCE > 32 cuando salió bien.
    r as isize > 32
}
