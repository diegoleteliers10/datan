//! Recursos de Windows: icono y metadatos de version en el .exe.
//!
//! Sin esto el ejecutable sale con el icono generico de Rust y, en
//! Propiedades > Detalles, sin nombre de producto ni version. Windows usa
//! esos campos en el Administrador de tareas y en el cuadro de "Aplicaciones"
//! de Configuracion, asi que no son cosmeticos.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/datan.ico");

    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("../../assets/datan.ico");
        res.set("ProductName", "Datan");
        res.set("FileDescription", "Datan");
        res.set("CompanyName", "Datan");
        res.set("LegalCopyright", "Copyright (c) 2026 Diego Letelier");
        res.set("OriginalFilename", "Datan.exe");
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        res.set("FileVersion", env!("CARGO_PKG_VERSION"));
        res.compile().expect("no se pudieron compilar los recursos de Windows");
    }
}
