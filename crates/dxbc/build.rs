//! Builds the vendored dxbc-spirv library (third_party/dxbc-spirv) and Aerugo's C interface over it.

use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "cpp") {
            out.push(path);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib = manifest.join("../../third_party/dxbc-spirv");
    let mut files = Vec::new();
    for dir in ["dxbc", "ir", "spirv", "util"] {
        sources(&lib.join(dir), &mut files);
    }
    files.push(manifest.join("shim/shim.cpp"));
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(&lib)
        .define("DXBC_SPV_ENABLE_SM5", None)
        .define("DXBC_SPV_ENABLE_SPIRV", None)
        .flag_if_supported("-fno-exceptions")
        .warnings(false)
        .files(&files);
    // The translator is slow unoptimised; build it optimised in every profile.
    build.opt_level(2);
    build.compile("aerugo_dxbc_spirv");
    println!("cargo:rerun-if-changed=shim/shim.cpp");
    println!("cargo:rerun-if-changed=../../third_party/dxbc-spirv");
}
