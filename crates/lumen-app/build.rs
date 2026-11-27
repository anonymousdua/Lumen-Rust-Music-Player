//! Embeds the app icon and version information into lumen.exe.

fn main() {
    println!("cargo:rerun-if-changed=resources/lumen.rc");
    println!("cargo:rerun-if-changed=../../assets/lumen.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("resources/lumen.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap_or_else(|e| panic!("cannot embed Windows resources: {e}"));
    }
}
