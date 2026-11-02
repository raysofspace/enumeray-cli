#[path = "build_support.rs"]
mod build_support;

fn main() {
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let source =
        build_support::source_identity(&root, "https://github.com/raysofspace/enumeray-cli");
    let output = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(
        output.join("source-identity.json"),
        serde_json::to_vec(&source).unwrap(),
    )
    .unwrap();
    println!(
        "cargo:rustc-env=ENUMERAY_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}
