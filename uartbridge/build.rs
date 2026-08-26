fn main() {
    println!("cargo:rustc-link-search={}", std::env::current_dir().unwrap().display());
    println!("cargo:rerun-if-changed=memory.x");
}
