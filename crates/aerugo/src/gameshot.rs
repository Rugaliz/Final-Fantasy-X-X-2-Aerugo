//! Local check of drawing with the games' own shaders: renders a map with its effects at a scene camera to a
//! PNG, and writes the frame in the oracle's capture form for comparison with the original's.
//!
//! Needs `AERUGO_GAME_DIR`, a Vulkan device and `AERUGO_GAMESHOT` = `map path;eye x,y,z;focus x,y,z;screen
//! distance;out.png` (game space). Optional: `AERUGO_GAMESHOT_SIZE` = `w,h` (default 2560,1440),
//! `AERUGO_GAMESHOT_MSAA` (default 8), `AERUGO_GAMESHOT_FOG` = `r,g,b;near;far;curve;limit`,
//! `AERUGO_GAMESHOT_BG` = `r,g,b`, `AERUGO_GAMESHOT_TIME`, `AERUGO_FRAME_DUMP` = capture file.

use std::collections::HashMap;

use aerugo_gfx::game::{GameFrame, GameOp, TargetDesc, TargetRef};
use aerugo_gfx::{ContextOptions, Renderer};
use aerugo_vfs::{Vfs, install_from_env};
use glam::{Mat4, Vec3};

use crate::phyre::model::{GameModel, main_pass, object_values, segment_draws};
use crate::phyre::scene::SceneConstants;
use crate::phyre::{Programs, dump_frame};

fn floats(s: &str) -> Vec<f32> {
    s.split(',').map(|v| v.trim().parse().unwrap()).collect()
}

fn point(s: &str) -> Vec3 {
    let v = floats(s);
    Vec3::new(v[0], v[1], v[2])
}

#[test]
fn game_shot() {
    let (Some(inst), Ok(spec)) = (install_from_env(), std::env::var("AERUGO_GAMESHOT")) else {
        eprintln!("skipped: set AERUGO_GAME_DIR and AERUGO_GAMESHOT");
        return;
    };
    let mut it = spec.split(';');
    let (path, eye, focus, d, out) = (
        it.next().unwrap(),
        point(it.next().unwrap()),
        point(it.next().unwrap()),
        it.next().unwrap(),
        it.next().unwrap(),
    );
    let d: f32 = d.parse().unwrap();
    let size = std::env::var("AERUGO_GAMESHOT_SIZE").map_or(vec![2560.0, 1440.0], |s| floats(&s));
    let (w, h) = (size[0] as u32, size[1] as u32);
    let msaa: u32 = std::env::var("AERUGO_GAMESHOT_MSAA").map_or(8, |s| s.parse().unwrap());
    let mut renderer =
        Renderer::new_headless(&ContextOptions { validation: true, preferred_gpu: None }).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let model = GameModel::load(&vfs, &mut renderer, path).unwrap();
    let mut scene = SceneConstants::camera(eye, focus, d, w as f32, h as f32);
    if let Ok(f) = std::env::var("AERUGO_GAMESHOT_FOG") {
        let p: Vec<&str> = f.split(';').collect();
        let c = floats(p[0]);
        scene.fog_color = [c[0], c[1], c[2]];
        scene.fog_near = p[1].parse().unwrap();
        scene.fog_far = p[2].parse().unwrap();
        scene.fog_curve = p[3].parse().unwrap();
        scene.fog_limit = p[4].parse().unwrap();
    }
    if let Ok(b) = std::env::var("AERUGO_GAMESHOT_BG") {
        let c = floats(&b);
        scene.background = [c[0], c[1], c[2]];
    }
    scene.time = std::env::var("AERUGO_GAMESHOT_TIME").map_or(0.0, |t| t.parse().unwrap());
    let color =
        renderer.create_target(&TargetDesc { width: w, height: h, dxgi_format: 28, samples: msaa }).unwrap();
    let normal =
        renderer.create_target(&TargetDesc { width: w, height: h, dxgi_format: 28, samples: msaa }).unwrap();
    let depth =
        renderer.create_target(&TargetDesc { width: w, height: h, dxgi_format: 44, samples: msaa }).unwrap();
    let resolved =
        renderer.create_target(&TargetDesc { width: w, height: h, dxgi_format: 28, samples: 1 }).unwrap();
    let bg = scene.background;
    let mut ops = vec![
        GameOp::ClearColor(TargetRef::Target(color), [bg[0], bg[1], bg[2], 1.0]),
        GameOp::ClearDepth(depth, 1.0, 0, 3),
        GameOp::ClearColor(TargetRef::Target(normal), [1.0; 4]),
        GameOp::SetTargets {
            colors: vec![TargetRef::Target(color), TargetRef::Target(normal)],
            depth: Some(depth),
        },
    ];
    let mut programs = Programs::default();
    let mut missing = Vec::new();
    let viewport = [0.0, 0.0, w as f32, h as f32, 0.0, 1.0];
    // The remaster's map instances are drawn ten times their file size (its World for them is 10 × the node's).
    let mut later = Vec::new();
    for mesh in &model.meshes {
        let source = &model.model.meshes[mesh.source];
        let world = Mat4::from_scale(Vec3::splat(10.0)) * Mat4::from_cols_array(&source.world);
        let object = object_values(world, scene.view, scene.projection);
        let values = |name: &str| scene.value(name).or_else(|| object(name));
        for seg in &mesh.segments {
            let fseg = &source.segments[seg.source];
            let Some(material) = source.materials.get(fseg.material as usize) else { continue };
            let Some((pass, transparent)) = main_pass(&vfs, model.game, material) else { continue };
            let draws = segment_draws(
                &vfs,
                &mut renderer,
                &mut programs,
                &model,
                mesh,
                seg,
                &pass,
                0,
                &values,
                4.0,
                viewport,
                &crate::phyre::model::Overrides::default(),
                &mut missing,
            );
            let draws = draws.into_iter().map(|d| GameOp::Draw(Box::new(d)));
            if transparent {
                later.extend(draws);
            } else {
                ops.extend(draws);
            }
        }
    }
    ops.extend(later);
    ops.push(GameOp::Resolve { src: color, dst: TargetRef::Target(resolved) });
    ops.push(GameOp::Copy { src: resolved, dst: TargetRef::Output });
    let frame = GameFrame { ops, transient: Vec::new() };
    let mut missing_count: HashMap<String, usize> = HashMap::new();
    for m in missing {
        *missing_count.entry(m).or_default() += 1;
    }
    let mut missing: Vec<_> = missing_count.into_iter().collect();
    missing.sort();
    eprintln!("not supplied: {missing:?}");
    eprintln!("problems: {:?}", programs.problems);
    if let Ok(dump) = std::env::var("AERUGO_FRAME_DUMP") {
        dump_frame(&frame, &programs.hashes(), &dump).unwrap();
    }
    let pixels = renderer.render_frame_offscreen(frame, (w, h)).unwrap();
    let file = std::fs::File::create(out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut wr| wr.write_image_data(&pixels)).unwrap();
    assert_eq!(aerugo_gfx::validation_error_count(), 0, "Vulkan validation errors");
}
