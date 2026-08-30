//! Windows resources for the executable: icon, version information, manifest.
//!
//! None of it changes what the program does, and all of it changes what the
//! program looks like to the person who downloaded it — a file with no icon and
//! an empty Properties dialog is the shape of something nobody wants to run.
//! The manifest earns its place separately; `assets/proxspace.manifest` says
//! why.
//!
//! A missing resource compiler is a warning rather than an error. `rc.exe`
//! comes with the Windows SDK and `windres` with a mingw toolchain; a machine
//! that has neither can still build a working `proxspace.exe`, only a plain
//! one, and failing the build over the icon would be the wrong trade.

fn main() {
    println!("cargo:rerun-if-changed=assets/proxspace.ico");
    println!("cargo:rerun-if-changed=assets/proxspace.manifest");

    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("assets/proxspace.ico")
        .set_manifest_file("assets/proxspace.manifest")
        // FileVersion and ProductVersion come from CARGO_PKG_VERSION.
        .set("ProductName", "ProxSpace")
        .set("FileDescription", env!("CARGO_PKG_DESCRIPTION"))
        .set("OriginalFilename", "proxspace.exe")
        .set("InternalName", "proxspace");

    if let Err(error) = resource.compile() {
        println!("cargo:warning=no Windows resources embedded ({error})");
    }
}
