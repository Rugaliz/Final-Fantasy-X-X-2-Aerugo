//! Golden-image harness for materials. Renders, for each of the most used effects, the model parts that use it
//! from a fixed camera, and compares the picture with a reference PNG.
//!
//! Local only: needs `AERUGO_GAME_DIR`, a Vulkan device and `AERUGO_GOLDEN_DIR` (a folder for the references,
//! which are game imagery and never committed). `AERUGO_GOLDEN_UPDATE=1` writes the current renders as the new
//! references. References may also be captures of the original game made with the same camera.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aerugo_formats::mesh;
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::{ContextOptions, Draw, Renderer, SceneView};
use aerugo_vfs::{Vfs, install_from_env};
use glam::{Mat4, Vec3};

use crate::assets::{self, GpuModel};
use crate::viewer::OrbitCamera;

const SIZE: u32 = 256;
const TOP_EFFECTS: usize = 20;
/// Mean absolute difference per channel (0–255) a render may have against its reference.
const MEAN_LIMIT: f64 = 2.0;
/// Share of pixels whose largest channel difference is over 32.
const OUTLIER_LIMIT: f64 = 0.005;

struct Case {
    effect: String,
    model: String,
    segments: usize,
}

/// The most used effects (by model segments), each with the model that uses it most.
fn top_effects(vfs: &Vfs) -> Vec<Case> {
    let mut per_effect: HashMap<String, (usize, HashMap<String, usize>)> = HashMap::new();
    let paths: Vec<String> = vfs
        .archives()
        .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
        .filter(|p| p.ends_with(".dae.phyre"))
        .collect();
    for path in paths {
        let Ok(c) = vfs
            .read(&path)
            .map_err(|e| e.to_string())
            .and_then(|d| Cluster::parse(d).map_err(|e| e.to_string()))
        else {
            continue;
        };
        let Ok(model) = mesh::read_model(&c) else { continue };
        for m in &model.meshes {
            for s in &m.segments {
                let Some(mat) = m.materials.get(s.material as usize) else { continue };
                let key = format!("{:?}:{}", assets::game_of_path(&path), mat.effect);
                let e = per_effect.entry(key).or_default();
                e.0 += 1;
                *e.1.entry(path.clone()).or_default() += 1;
            }
        }
    }
    let mut ranked: Vec<_> = per_effect.into_iter().collect();
    ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .take(TOP_EFFECTS)
        .map(|(key, (n, models))| {
            let model =
                models.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|m| m.0).unwrap();
            let effect = key.split_once(':').map(|k| k.1.to_owned()).unwrap_or(key);
            Case { effect, model, segments: n }
        })
        .collect()
}

/// Draws of the parts that use `effect`, and their bounds.
fn effect_draws(cluster: &Cluster, model: &GpuModel, effect: &str) -> (Vec<Draw>, Vec3, Vec3) {
    let (mut draws, mut min, mut max) = (Vec::new(), Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for m in &model.meshes {
        let source = &model.model.meshes[m.source];
        for (part, fx) in m.part_effects.iter().enumerate() {
            if fx != effect {
                continue;
            }
            draws.push(Draw {
                mesh: m.mesh,
                model: m.world.to_cols_array(),
                joint_base: None,
                lit: false,
                part: Some(part),
            });
            let seg = &source.segments[m.part_segments[part]];
            if let Some(pos) = seg.positions() {
                for p in mesh::stream_floats(cluster, pos).chunks_exact(3) {
                    let w = m.world.transform_point3(Vec3::new(p[0], p[1], p[2]));
                    min = min.min(w);
                    max = max.max(w);
                }
            }
        }
    }
    if min.x > max.x {
        (min, max) = (model.min, model.max);
    }
    (draws, min, max)
}

fn view(draws: Vec<Draw>, min: Vec3, max: Vec3, culling: bool, mirrored: bool) -> SceneView {
    let cam = OrbitCamera::framing(min, max);
    let view_proj: Mat4 = cam.view_proj(1.0);
    SceneView {
        view_proj: view_proj.to_cols_array(),
        clear: [0.2, 0.2, 0.25, 1.0],
        draws,
        joints: Vec::new(),
        culling,
        mirrored,
        fog: None,
        time: 0.0,
        color_correction: None,
        glow: None,
        character_light: None,
    }
}

/// (mean absolute channel difference, share of pixels with a channel off by more than 32).
fn compare(a: &[u8], b: &[u8]) -> (f64, f64) {
    let (mut sum, mut outliers) = (0u64, 0usize);
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let mut worst = 0;
        for k in 0..3 {
            let d = pa[k].abs_diff(pb[k]);
            sum += u64::from(d);
            worst = worst.max(d);
        }
        if worst > 32 {
            outliers += 1;
        }
    }
    let pixels = (a.len() / 4).max(1);
    (sum as f64 / (pixels * 3) as f64, outliers as f64 / pixels as f64)
}

fn write_png(path: &Path, rgba: &[u8]) {
    let file = std::fs::File::create(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), SIZE, SIZE);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut w| w.write_image_data(rgba)).unwrap();
}

fn read_png(path: &Path) -> Option<Vec<u8>> {
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    (info.color_type == png::ColorType::Rgba && info.width == SIZE && info.height == SIZE)
        .then(|| buf[..info.buffer_size()].to_vec())
}

fn file_name(i: usize, effect: &str) -> String {
    let family = aerugo_formats::effect::effect_family(effect).to_ascii_lowercase();
    let variant = effect.rsplit('#').next().unwrap_or("").to_ascii_lowercase();
    format!("{i:02}-{family}-{}.png", &variant[..variant.len().min(8)])
}

#[test]
fn top_effects_match_references() {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set AERUGO_GAME_DIR");
        return;
    };
    let Some(dir) = std::env::var_os("AERUGO_GOLDEN_DIR").map(PathBuf::from) else {
        eprintln!("skipped: set AERUGO_GOLDEN_DIR to a folder for reference images");
        return;
    };
    let update = std::env::var("AERUGO_GOLDEN_UPDATE").is_ok_and(|v| v == "1");
    let mut renderer = match Renderer::new_headless(&ContextOptions { validation: true, preferred_gpu: None })
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skipped: no usable Vulkan device ({e:#})");
            return;
        }
    };
    std::fs::create_dir_all(&dir).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let cases = top_effects(&vfs);
    assert_eq!(cases.len(), TOP_EFFECTS);
    let (mut failures, mut winding_ok, mut winding_flipped) = (Vec::new(), 0.0, 0.0);
    for (i, case) in cases.iter().enumerate() {
        let model = assets::load_model(&vfs, &mut renderer, &case.model)
            .unwrap_or_else(|e| panic!("{}: {e}", case.model));
        let cluster = Cluster::parse(vfs.read(&case.model).unwrap()).unwrap();
        let (draws, min, max) = effect_draws(&cluster, &model, &case.effect);
        let render = |r: &mut Renderer, culling, mirrored| {
            r.render_offscreen(view(draws.clone(), min, max, culling, mirrored), (SIZE, SIZE)).unwrap()
        };
        let image = render(&mut renderer, true, false);
        // Culling with the right winding removes only hidden faces; the wrong winding removes visible ones.
        let both_sides = render(&mut renderer, false, false);
        let flipped = render(&mut renderer, true, true);
        winding_ok += compare(&image, &both_sides).0;
        winding_flipped += compare(&flipped, &both_sides).0;
        let name = file_name(i, &case.effect);
        let reference = dir.join(&name);
        match read_png(&reference) {
            Some(want) if !update => {
                let (mean, outliers) = compare(&image, &want);
                eprintln!(
                    "{name}: mean {mean:.3}, outliers {:.3}%  ({} segments; {})",
                    outliers * 100.0,
                    case.segments,
                    case.model
                );
                if mean > MEAN_LIMIT || outliers > OUTLIER_LIMIT {
                    write_png(&dir.join(format!("{name}.actual.png")), &image);
                    failures.push(format!("{name}: mean {mean:.3}, outliers {:.3}%", outliers * 100.0));
                }
            }
            _ if update => {
                write_png(&reference, &image);
                eprintln!("{name}: written ({} segments; {})", case.segments, case.model);
            }
            _ => failures.push(format!("{name}: no reference (run with AERUGO_GOLDEN_UPDATE=1)")),
        }
        model.destroy(&mut renderer);
    }
    eprintln!("winding: culled vs both sides {winding_ok:.2}, with flipped winding {winding_flipped:.2}");
    assert!(winding_ok < winding_flipped, "front faces are being culled: the winding convention is wrong");
    assert_eq!(aerugo_gfx::validation_error_count(), 0, "Vulkan validation errors");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
