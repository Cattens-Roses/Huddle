fn main() {
    println!("cargo:rerun-if-env-changed=HUDDLE_BUILD_VERSION");
    if let Ok(version) = std::env::var("HUDDLE_BUILD_VERSION") {
        if !version.is_empty() {
            println!("cargo:rustc-env=HUDDLE_BUILD_VERSION={version}");
        }
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources();
    }
}

fn embed_windows_resources() {
    use std::{env, fs, path::PathBuf};

    let icon = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../assets/windows/huddle.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let icon = icon.canonicalize().expect("Huddle icon is missing");
    let icon = icon
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numeric = format!(
        "{},{},{},0",
        env::var("CARGO_PKG_VERSION_MAJOR").unwrap(),
        env::var("CARGO_PKG_VERSION_MINOR").unwrap(),
        env::var("CARGO_PKG_VERSION_PATCH").unwrap()
    );
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("huddle.rc");
    fs::write(
        &resource,
        format!(
            r#"
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Huddle\0"
            VALUE "FileDescription", "Huddle\0"
            VALUE "FileVersion", "{version}\0"
            VALUE "InternalName", "Huddle\0"
            VALUE "OriginalFilename", "Huddle.exe\0"
            VALUE "ProductName", "Huddle\0"
            VALUE "ProductVersion", "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
        ),
    )
    .expect("cannot write Huddle resources");
    embed_resource::compile(&resource, embed_resource::NONE)
        .manifest_required()
        .expect("cannot embed Huddle icon and Windows metadata");
}
