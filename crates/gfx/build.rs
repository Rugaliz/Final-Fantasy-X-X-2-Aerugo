//! Compiles the GLSL shaders in `shaders/` to SPIR-V with naga (pure Rust, so no Vulkan SDK is needed).

use std::path::Path;

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    for entry in std::fs::read_dir("shaders").expect("shaders directory") {
        let path = entry.expect("dir entry").path();
        let stage = match path.extension().and_then(|e| e.to_str()) {
            Some("vert") => naga::ShaderStage::Vertex,
            Some("frag") => naga::ShaderStage::Fragment,
            Some("comp") => naga::ShaderStage::Compute,
            _ => continue,
        };
        println!("cargo:rerun-if-changed={}", path.display());
        let source = std::fs::read_to_string(&path).expect("read shader");
        let words = compile(&path, &source, stage);
        let name = path.file_name().unwrap().to_str().unwrap();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        std::fs::write(Path::new(&out_dir).join(format!("{name}.spv")), bytes).expect("write SPIR-V");
    }
    println!("cargo:rerun-if-changed=shaders");
}

fn compile(path: &Path, source: &str, stage: naga::ShaderStage) -> Vec<u32> {
    let module = naga::front::glsl::Frontend::default()
        .parse(&naga::front::glsl::Options::from(stage), source)
        .unwrap_or_else(|e| panic!("{}: {}", path.display(), e.emit_to_string(source)));
    let info =
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}: {}", path.display(), e.emit_to_string(source)));
    let pipeline = naga::back::spv::PipelineOptions { shader_stage: stage, entry_point: "main".into() };
    // GLSL sources here are written for Vulkan's coordinate system; naga's default would flip Y a second time.
    let mut options = naga::back::spv::Options::default();
    options.flags.remove(naga::back::spv::WriterFlags::ADJUST_COORDINATE_SPACE);
    naga::back::spv::write_vec(&module, &info, &options, Some(&pipeline))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
