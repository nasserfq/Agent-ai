//! Puts the app icon into the Windows executable. Nothing to do on other systems.

fn main() {
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon/agent-ai.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not add the Windows icon: {e}");
        }
    }
}
