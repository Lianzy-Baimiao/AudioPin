fn main() {
    for path in ["assets/app.rc", "assets/app.ico", "assets/app.manifest"] {
        println!("cargo:rerun-if-changed={path}");
    }
    embed_resource::compile("assets/app.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
