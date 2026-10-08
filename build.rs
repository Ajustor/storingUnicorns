fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // Gives the Windows executable its icon (Explorer, taskbar). Checked against
    // the target, not the host, so cross builds work.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(e) = res.compile() {
            // A missing resource compiler must not break the build.
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
