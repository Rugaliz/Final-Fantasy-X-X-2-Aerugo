//! Uploads game textures with validation enabled (runs when `AERUGO_GAME_DIR` is set and a Vulkan device
//! exists). Every 40th texture cluster of both games.

use aerugo_formats::phyre::Cluster;
use aerugo_formats::texture::{PixelFormat, read_textures};
use aerugo_gfx::{Context, ContextOptions, TexFormat, Texture, TextureDesc};
use aerugo_vfs::{Vfs, install_from_env};

fn tex_format(f: PixelFormat) -> TexFormat {
    match f {
        PixelFormat::Bc1 => TexFormat::Bc1,
        PixelFormat::Bc2 => TexFormat::Bc2,
        PixelFormat::Bc3 => TexFormat::Bc3,
        PixelFormat::Rgba8 => TexFormat::Rgba8,
        PixelFormat::Bgra8 => TexFormat::Bgra8,
        PixelFormat::R8 => TexFormat::R8,
        other => panic!("format {other:?} does not occur in the games"),
    }
}

#[test]
fn game_textures_upload() {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: no game installation (AERUGO_GAME_DIR)");
        return;
    };
    let ctx = match Context::new_headless(&ContextOptions { validation: true, preferred_gpu: None }) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skipped: no usable Vulkan device ({e:#})");
            return;
        }
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap_or_else(|e| panic!("{e}"));
    let mut uploaded = 0;
    for (_, archive) in vfs.archives() {
        for e in archive.entries().iter().filter(|e| e.path.ends_with(".dds.phyre")).step_by(40) {
            let Ok(c) = Cluster::parse(archive.read(e).unwrap()) else { continue };
            for t in read_textures(&c) {
                let t = t.unwrap_or_else(|err| panic!("{}: {err}", e.path));
                let data = c.texture_data();
                let desc = TextureDesc {
                    format: tex_format(t.format.unwrap()),
                    width: t.width,
                    height: t.height,
                    levels: t.levels.len() as u32 / t.faces,
                    layers: t.faces,
                    cube: t.faces == 6,
                    data: t.levels.iter().map(|r| &data[r.clone()]).collect(),
                    name: &e.path,
                };
                let mut tex =
                    Texture::create(&ctx, &desc).unwrap_or_else(|err| panic!("{}: {err:#}", e.path));
                // SAFETY: the upload waited for completion and nothing else uses the texture.
                unsafe { tex.destroy(&ctx) };
                uploaded += 1;
            }
        }
    }
    eprintln!("uploaded {uploaded} textures");
    assert_eq!(aerugo_gfx::validation_error_count(), 0, "validation errors (see stderr)");
    assert!(uploaded > 2000);
}
