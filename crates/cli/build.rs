//! Rebuild when the board build changes, so a release binary never keeps a
//! stale or empty embed after `board/dist` is rebuilt. Debug builds read the
//! board from disk at run time, so they never need to rebuild for it.
//!
//! Cargo treats a missing `rerun-if-changed` path as always stale, so only
//! paths that exist are watched.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("PROFILE").is_ok_and(|profile| profile == "release") {
        let dist = Path::new("../../board/dist");
        if dist.join("index.html").exists() {
            println!("cargo:rerun-if-changed=../../board/dist");
        } else {
            if Path::new("../../board").exists() {
                println!("cargo:rerun-if-changed=../../board");
            }
            println!("cargo:warning=board/dist is missing: this binary will not serve the board");
        }
    }
}
