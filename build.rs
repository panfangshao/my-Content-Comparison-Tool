//! Windows resource metadata: the exe icon plus the version block Explorer
//! shows in Properties. A no-op on every other platform.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "DuiBi");
        res.set("FileDescription", "DuiBi - text compare & merge");
        res.set("LegalCopyright", "MIT licensed");

        // The icon is optional so a fresh clone builds without it.
        if std::path::Path::new("assets/icon.ico").exists() {
            res.set_icon("assets/icon.ico");
        }

        if let Err(e) = res.compile() {
            // Missing rc.exe on a minimal toolchain should not block the build.
            println!("cargo:warning=skipping Windows resources: {e}");
        }
    }
}
