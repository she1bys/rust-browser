//! Builds the Windows resources into the .exe: the app icon (shown in
//! Explorer, the taskbar and the installer) and the version details shown in
//! the file's Properties > Details.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    let mut res = winresource::WindowsResource::new();
    // Resource id 1: `main.rs` loads it for the window and taskbar icon.
    res.set_icon_with_id("assets/icon.ico", "1")
        .set("ProductName", "Rust Browser")
        .set("FileDescription", "Rust Browser")
        .set("InternalName", "rust-browser")
        .set("OriginalFilename", "rust-browser.exe")
        .set("ProductVersion", version)
        .set("FileVersion", version)
        .set("LegalCopyright", "Copyright (c) 2026 Rust Browser contributors. MIT License.");
    if let Err(e) = res.compile() {
        panic!("could not build the Windows resources (icon, version info): {e}");
    }
}
