// daemonとshimが、どちらが新しいbuildかを比べるための刻印（§2.2）。
fn main() {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    println!("cargo:rustc-env=AM_BUILD_STAMP={stamp}");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../am-tools/src");
    println!("cargo:rerun-if-changed=../am-store/src");
    println!("cargo:rerun-if-changed=../am-domain/src");
    println!("cargo:rerun-if-changed=../am-anchor/src");
    println!("cargo:rerun-if-changed=../am-embed/src");
}
