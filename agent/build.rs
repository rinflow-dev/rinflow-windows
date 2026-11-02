// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("rinflow-agent.manifest");
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

    version_info();
}

/// The version resource: its FileDescription is the name Windows shows for
/// this program on its notifications, in Task Manager and in the firewall
/// prompt ("rinflow-agent.exe" without it).
fn version_info() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("package version");
    let mut parts = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u16>().unwrap_or(0));
    let mut number = || parts.next().unwrap_or(0);
    let (major, minor, patch) = (number(), number(), number());
    // Numbers instead of winver.h names, so rc.exe needs no include path:
    // VOS_NT_WINDOWS32, VFT_APP; U.S. English, Unicode.
    let script = format!(
        r#"#pragma code_page(65001)
101 ICON "{icon}"
102 ICON "{dark_icon}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Rinflow"
      VALUE "FileDescription", "Rinflow"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "rinflow-agent"
      VALUE "OriginalFilename", "rinflow-agent.exe"
      VALUE "ProductName", "Rinflow"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon_path("Rinflow.ico"),
        dark_icon = icon_path("Rinflow.Dark.ico"),
    );
    let path = PathBuf::from(std::env::var("OUT_DIR").expect("out dir")).join("version.rc");
    std::fs::write(&path, script).expect("write version.rc");
    embed_resource::compile(&path, embed_resource::NONE)
        .manifest_optional()
        .expect("compile the version resource");
}

fn icon_path(name: &str) -> String {
    let path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("../app/Assets")
        .join(name);
    println!("cargo:rerun-if-changed={}", path.display());
    path.to_string_lossy().replace('\\', "/")
}
