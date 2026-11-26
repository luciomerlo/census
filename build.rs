//! Metadatos del ejecutable en Windows: sin esto el `.exe` sale sin icono y
//! el Explorador y la barra de tareas muestran el genérico.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(std::ffi::OsStr::new("windows")) {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("ProductName", "CensusIndex");
    res.set("FileDescription", "CensusIndex - indice instantaneo de archivos");
    res.set("CompanyName", "Lucio Merlo");
    res.set("LegalCopyright", "MIT");
    res.set("OriginalFilename", "censusindex.exe");
    if let Err(e) = res.compile() {
        // No romper el build por el icono: se avisa y se sigue.
        println!("cargo:warning=no se pudo embeber el icono: {e}");
    }
}
