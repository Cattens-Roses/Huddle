fn main() {
    println!("cargo:rerun-if-env-changed=HUDDLE_BUILD_VERSION");
    if let Ok(version) = std::env::var("HUDDLE_BUILD_VERSION") {
        if !version.is_empty() {
            println!("cargo:rustc-env=HUDDLE_BUILD_VERSION={version}");
        }
    }
}
