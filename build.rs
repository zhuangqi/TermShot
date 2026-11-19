fn main() {
    println!("cargo:rerun-if-changed=assets/TermShot.rc");
    println!("cargo:rerun-if-changed=assets/TermShot.ico");
    println!("cargo:rerun-if-changed=assets/TermShot.manifest");
    embed_resource::compile("assets/TermShot.rc", embed_resource::NONE)
        .manifest_required()
        .expect("could not embed the Windows application icon and manifest");
}
