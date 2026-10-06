// Builds the app icon, version info and Windows manifest into desktop-dock.exe
// (uses rc.exe from the Windows SDK that comes with Visual Studio's C++ tools).
fn main() {
    println!("cargo:rerun-if-changed=assets/desktop-dock.rc");
    println!("cargo:rerun-if-changed=assets/desktop-dock.manifest");
    println!("cargo:rerun-if-changed=assets/dock.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    // The version in one place (Cargo.toml): the rc's VERSIONINFO uses these.
    let part = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let macros = [
        format!("VER_MAJOR={}", part("CARGO_PKG_VERSION_MAJOR")),
        format!("VER_MINOR={}", part("CARGO_PKG_VERSION_MINOR")),
        format!("VER_PATCH={}", part("CARGO_PKG_VERSION_PATCH")),
        format!("VER_STRING=\"{version}\""),
    ];
    embed_resource::compile("assets/desktop-dock.rc", &macros)
        .manifest_required()
        .expect("couldn't build the Windows resources (icon, manifest)");
    // The maintainer's own test notes (testing\, not published) add a few checks to `cargo test`.
    println!("cargo:rustc-check-cfg=cfg(dev_lab)");
    if std::path::Path::new("testing/inventory.md").exists() {
        println!("cargo:rerun-if-changed=testing/inventory.md");
        println!("cargo:rustc-cfg=dev_lab");
    }
}
