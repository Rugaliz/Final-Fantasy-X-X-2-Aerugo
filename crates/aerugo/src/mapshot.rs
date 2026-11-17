//! Local check of map rendering against captures of the original: renders a map (and optionally a character)
//! from a scene camera to a PNG, with the game screen's map placement, projection and aspect policy.
//!
//! Needs `AERUGO_GAME_DIR`, a Vulkan device and `AERUGO_SHOT` = `map path;eye x,y,z;focus x,y,z;screen
//! distance;out.png` (game-space points, as the recordings give them). Skipped without them. Optional:
//! `AERUGO_SHOT_CHR` = `model path;x,y,z;yaw` adds a character in its default pose; `AERUGO_SHOT_SWEEP` =
//! `d,d,…;ty,ty,…` renders one picture per screen distance and vertical clip offset (out-<d>-<ty>.png), for
//! fitting the projection to a capture; `AERUGO_SHOT_CAMS` = a file of `name;eye;focus;d` lines renders one
//! picture per camera (out-<name>.png).

use aerugo_gfx::aspect::{AspectMode, clip_transform};
use aerugo_gfx::{ContextOptions, Draw, Renderer, SceneView};
use aerugo_vfs::{Vfs, install_from_env};
use glam::{Mat4, Vec3};

use crate::assets;
use crate::viewer::field::{FIELD_FRAME, field_fov_y, map_place, script_camera};

const SIZE: (u32, u32) = (1280, 720);

/// One picture to render: name tag, eye, focus, screen distance, vertical clip offset.
type Variant = (String, [f32; 3], [f32; 3], f32, f32);

fn point(s: &str) -> [f32; 3] {
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse().unwrap()).collect();
    [v[0], v[1], v[2]]
}

fn list(s: &str) -> Vec<f32> {
    s.split(',').map(|x| x.trim().parse().unwrap()).collect()
}

#[test]
fn map_shot() {
    let (Some(inst), Ok(spec)) = (install_from_env(), std::env::var("AERUGO_SHOT")) else {
        eprintln!("skipped: set AERUGO_GAME_DIR and AERUGO_SHOT");
        return;
    };
    let parts: Vec<&str> = spec.split(';').collect();
    let (map, eye, focus, out) = (parts[0], point(parts[1]), point(parts[2]), parts[4]);
    let screen: f32 = parts[3].parse().unwrap();
    let mut renderer =
        Renderer::new_headless(&ContextOptions { validation: false, preferred_gpu: None }).unwrap();
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let model = assets::load_map_model(&vfs, &mut renderer, map).unwrap();
    // AERUGO_SHOT_SKIP = mesh indices to leave out (for telling layers apart).
    let skip: Vec<usize> = std::env::var("AERUGO_SHOT_SKIP")
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_default();
    let mut draws: Vec<Draw> = model
        .meshes
        .iter()
        .enumerate()
        .filter(|(i, _)| !skip.contains(i))
        .map(|(_, m)| Draw {
            mesh: m.mesh,
            model: (map_place() * m.world).to_cols_array(),
            joint_base: None,
            lit: false,
            part: None,
        })
        .collect();
    let mut joints: Vec<[f32; 16]> = Vec::new();
    if let Ok(c) = std::env::var("AERUGO_SHOT_CHR") {
        let mut it = c.split(';');
        let path = it.next().unwrap();
        let at = point(it.next().unwrap());
        let yaw: f32 = it.next().map_or(0.0, |y| y.parse().unwrap());
        let m = assets::load_model(&vfs, &mut renderer, path).unwrap();
        // As on the game screen: Y points down, so a turn about Y is negated.
        let place = Mat4::from_translation(Vec3::from(at)) * Mat4::from_rotation_y(-yaw);
        for mesh in &m.meshes {
            match (&mesh.skeleton, mesh.skinned) {
                (Some(sk), true) => {
                    let world = sk.world(&sk.default_pose);
                    for (pi, &si) in mesh.part_segments.iter().enumerate() {
                        let seg = &m.model.meshes[mesh.source].segments[si];
                        let base = joints.len() as u32;
                        joints.extend(sk.palette(&world, seg));
                        draws.push(Draw {
                            mesh: mesh.mesh,
                            model: (place * mesh.world).to_cols_array(),
                            joint_base: Some(base),
                            lit: true,
                            part: Some(pi),
                        });
                    }
                }
                _ => draws.push(Draw {
                    mesh: mesh.mesh,
                    model: (place * mesh.world).to_cols_array(),
                    joint_base: None,
                    lit: false,
                    part: None,
                }),
            }
        }
    }
    let extent = (model.max - model.min).length() * 10.0;
    // AERUGO_SHOT_FOG = `distance scale,limit mode (a: l/255, b: (1 − l/255)·0.4, c: 1 − l/255),curve`: the map
    // package's fog converted that way (for fitting the conversion to captures); unset draws no fog.
    let fog = std::env::var("AERUGO_SHOT_FOG").ok().and_then(|spec| {
        let mf = aerugo_sim::field::walk_mesh_path(map)
            .and_then(|p| vfs.read(&p).ok())
            .and_then(|d| aerugo_sim::field::MapFog::from_map_package(&d))?;
        let f: Vec<&str> = spec.split(',').collect();
        let k: f32 = f[0].parse().unwrap();
        let l = mf.limit / 255.0;
        let limit = match f[1] {
            "a" => l,
            "b" => (1.0 - l) * 0.4,
            _ => 1.0 - l,
        };
        eprintln!("fog {mf:?} -> limit {limit}");
        Some(aerugo_gfx::Fog {
            color: mf.color.map(|c| f32::from(c) / 255.0),
            near: mf.near * k,
            far: mf.far * k,
            curve: f[2].parse().unwrap(),
            limit,
        })
    });
    let aspect = SIZE.0 as f32 / SIZE.1 as f32;
    let sweep = std::env::var("AERUGO_SHOT_SWEEP").ok();
    // AERUGO_SHOT_CAMS = a file of `name;eye;focus;d` lines: one picture per camera (out-<name>.png).
    let cams = std::env::var("AERUGO_SHOT_CAMS").ok().map(|f| std::fs::read_to_string(f).unwrap());
    let mut variants: Vec<Variant> = match &sweep {
        Some(sw) => {
            let (ds, ts) = sw.split_once(';').unwrap();
            list(ds)
                .into_iter()
                .flat_map(|d| list(ts).into_iter().map(move |t| (format!("{d}-{t}"), eye, focus, d, t)))
                .collect()
        }
        None => vec![(String::new(), eye, focus, screen, 0.0)],
    };
    if let Some(c) = &cams {
        variants = c
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let f: Vec<&str> = l.split(';').collect();
                (f[0].to_owned(), point(f[1]), point(f[2]), f[3].parse().unwrap(), 0.0)
            })
            .collect();
    }
    for (tag, eye, focus, d, ty) in variants {
        let mut c = clip_transform(FIELD_FRAME, aspect, AspectMode::Crop, [0.0, 0.0]);
        c.ty += ty;
        let view_proj =
            Mat4::from_cols_array(&c.matrix()) * script_camera(eye, focus, field_fov_y(d), 0.0, extent);
        let rgba = renderer
            .render_offscreen(
                SceneView {
                    view_proj: view_proj.to_cols_array(),
                    clear: [0.0, 0.0, 0.0, 1.0],
                    draws: draws.clone(),
                    joints: joints.clone(),
                    culling: true,
                    mirrored: false,
                    fog,
                    time: std::env::var("AERUGO_SHOT_TIME").ok().and_then(|t| t.parse().ok()).unwrap_or(0.0),
                },
                SIZE,
            )
            .unwrap();
        let name = if tag.is_empty() {
            out.to_owned()
        } else {
            format!("{}-{tag}.png", out.trim_end_matches(".png"))
        };
        let file = std::fs::File::create(name).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), SIZE.0, SIZE.1);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().and_then(|mut w| w.write_image_data(&rgba)).unwrap();
    }
}
