// Embeds the application icon (assets/shelf.ico, made by tools/make-icon.ps1).
fn main() {
    println!("cargo:rerun-if-changed=assets/shelf.rc");
    println!("cargo:rerun-if-changed=assets/shelf.ico");
    embed_resource::compile("assets/shelf.rc", embed_resource::NONE).manifest_optional().unwrap();
}
