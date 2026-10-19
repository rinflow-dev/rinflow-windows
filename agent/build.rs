use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("linkcursor-agent.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());

    // Per-monitor DPI awareness must be declared in the manifest; coordinates
    // from hooks and SendInput are virtualized for DPI-unaware processes.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
