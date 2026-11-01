//! Rebuild when the board build changes, so a release binary never keeps a
//! stale or empty embed after `board/dist` is rebuilt.

fn main() {
    println!("cargo:rerun-if-changed=../../board/dist");
    if std::env::var("PROFILE").is_ok_and(|profile| profile == "release")
        && !std::path::Path::new("../../board/dist/index.html").exists()
    {
        println!("cargo:warning=board/dist is missing: this binary will not serve the board");
    }
}
