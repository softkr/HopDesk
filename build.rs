fn main() {
    println!("cargo:rerun-if-changed=assets/icon/HopDesk.ico");
    // Embed the app icon into hopdesk.exe (shown in Explorer and the taskbar).
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/HopDesk.ico");
        res.compile().expect("compile Windows resources");
    }
}
