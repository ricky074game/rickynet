//! Embeds the Windows resources (requireAdministrator manifest + app icon) into
//! rickynet.exe. Only invoked when the *target* OS is Windows; on other hosts
//! (e.g. the Linux CI job that unit-tests the portable modules) it is a no-op.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        println!("cargo:rerun-if-changed=rickynet.rc");
        println!("cargo:rerun-if-changed=app.manifest");
        println!("cargo:rerun-if-changed=../assets/icon.ico");
        embed_resource::compile("rickynet.rc", embed_resource::NONE);
    }
}
