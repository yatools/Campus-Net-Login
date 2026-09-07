fn main() {
    println!("cargo:rerun-if-changed=resources/app.rc");
    println!("cargo:rerun-if-changed=resources/app.manifest");
    println!("cargo:rerun-if-changed=resources/assets/campus-blue.ico");
    println!("cargo:rerun-if-changed=resources/assets/campus-red.ico");

    embed_resource::compile("resources/app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to compile Windows resources");
}
