fn main() {
    println!("cargo:rerun-if-changed=assets/CliScreenshot.rc");
    println!("cargo:rerun-if-changed=assets/CliScreenshot.ico");
    println!("cargo:rerun-if-changed=assets/CliScreenshot.manifest");
    embed_resource::compile("assets/CliScreenshot.rc", embed_resource::NONE)
        .manifest_required()
        .expect("could not embed the Windows application icon and manifest");
}
