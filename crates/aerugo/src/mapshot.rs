//! Local check of map rendering against captures of the original: renders a map (and optionally a character)
//! from a scene camera to a PNG, with the game screen's map placement, projection and aspect policy.
//!
//! Needs `AERUGO_GAME_DIR`, a Vulkan device and `AERUGO_SHOT` = `map path;eye x,y,z;focus x,y,z;screen
//! distance;out.png` (game-space points, as the recordings give them). Skipped without them. Optional:
//! `AERUGO_SHOT_CHR` = `model path;x,y,z;yaw` adds a character in its default pose (`AERUGO_SHOT_CHR_MOTION` =
//! a motion index poses it at that motion's first frame; `AERUGO_DEBUG_WORLD` prints its matrices); `AERUGO_SHOT_SWEEP` =
//! `d,d,…;ty,ty,…` renders one picture per screen distance and vertical clip offset (out-<d>-<ty>.png), for
//! fitting the projection to a capture; `AERUGO_SHOT_CAMS` = a file of `name;eye;focus;d` lines renders one
//! picture per camera (out-<name>.png); `AERUGO_SHOT_SIZE` = `w,h` sets the picture size; `AERUGO_SHOT_NOCULL`
//! draws both sides of everything; `AERUGO_SHOT_CC` applies the map's colour-correction table;
//! `AERUGO_SHOT_VALIDATE` runs under the Vulkan validation layer; `AERUGO_SHOT_MSAA` sets the samples per pixel;
//! `AERUGO_SHOT_NOSHARPEN` leaves out the unsharp mask; `AERUGO_SHOT_NOGLOW` the glow; `AERUGO_SHOT_HDAO` sets the ambient occlusion level;
//! `AERUGO_SHOT_LIGHTS` = `index,r,g,b,yaw,pitch;…` sets character lights as the scripts do (`AERUGO_SHOT_NOLIGHT`: none);
//! `AERUGO_SHOT_BRIGHTNESS` the brightness setting.

use aerugo_gfx::aspect::{AspectMode, clip_transform};
use aerugo_gfx::{ContextOptions, Draw, Renderer, SceneView};
use aerugo_vfs::{Vfs, install_from_env};
use glam::Mat4;

use crate::assets;
use crate::viewer::field::{FIELD_FRAME, character_place, field_fov_y, map_place, script_camera};

/// The field light as an event start leaves it, with `AERUGO_SHOT_LIGHTS` = `index,r,g,b,yaw,pitch;…` set over it
/// as native 4013 would; `AERUGO_SHOT_NOLIGHT` leaves characters unlit.
fn character_light(glow: Option<aerugo_gfx::Glow>, map: &str) -> Option<aerugo_gfx::CharacterLight> {
    if std::env::var("AERUGO_SHOT_NOLIGHT").is_ok() {
        return None;
    }
    let mut lights = aerugo_sim::field::FieldLights::default();
    for spec in std::env::var("AERUGO_SHOT_LIGHTS").unwrap_or_default().split(';').filter(|s| !s.is_empty()) {
        let v: Vec<i32> = spec.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if let [i, r, g, b, yaw, pitch] = v[..]
            && let Some(l) = lights.0.get_mut(i as usize)
        {
            *l = aerugo_sim::field::FieldLight::from_script(r, g, b, yaw, pitch);
        }
    }
    Some(assets::character_light(&lights, glow, map))
}

/// Picture size; `AERUGO_SHOT_SIZE` = `w,h` overrides it (captures of the original are 2560×1440).
fn size() -> (u32, u32) {
    std::env::var("AERUGO_SHOT_SIZE")
        .ok()
        .and_then(|s| {
            s.split_once(',').and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
        })
        .unwrap_or((1280, 720))
}

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
    // AERUGO_SHOT_VALIDATE runs with the Vulkan validation layer and fails on any error it reports.
    let validate = std::env::var("AERUGO_SHOT_VALIDATE").is_ok();
    let mut renderer =
        Renderer::new_headless(&ContextOptions { validation: validate, preferred_gpu: None }).unwrap();
    // AERUGO_SHOT_MSAA = samples per pixel (default 8, the remaster's high setting).
    let msaa = std::env::var("AERUGO_SHOT_MSAA").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    renderer.set_msaa(msaa).unwrap();
    // The unsharp mask is on as in the captures; AERUGO_SHOT_NOSHARPEN turns it off.
    renderer.set_sharpen(std::env::var("AERUGO_SHOT_NOSHARPEN").is_err());
    // AERUGO_SHOT_BRIGHTNESS = the brightness setting (0–100, 50 as the game).
    renderer.set_brightness(
        std::env::var("AERUGO_SHOT_BRIGHTNESS").ok().and_then(|v| v.parse().ok()).unwrap_or(50),
    );
    // AERUGO_SHOT_HDAO = ambient occlusion level (0 off … 3 high, the default as in the captures).
    renderer.set_hdao(std::env::var("AERUGO_SHOT_HDAO").ok().and_then(|v| v.parse().ok()).unwrap_or(3));
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
            tint: None,
        })
        .collect();
    let mut joints: Vec<[f32; 16]> = Vec::new();
    if let Ok(c) = std::env::var("AERUGO_SHOT_CHR") {
        let mut it = c.split(';');
        let path = it.next().unwrap();
        let at = point(it.next().unwrap());
        let yaw: f32 = it.next().map_or(0.0, |y| y.parse().unwrap());
        let m = assets::load_model(&vfs, &mut renderer, path).unwrap();
        let place = character_place(at, yaw);
        // AERUGO_SHOT_CHR_ALPHA draws the character faded (its alpha, as native 5033 sets it).
        let tint = std::env::var("AERUGO_SHOT_CHR_ALPHA")
            .ok()
            .and_then(|a| a.parse().ok())
            .map(|alpha| aerugo_gfx::CharacterTint { material: [1.0; 3], shade: [1.0; 3], alpha });
        for mesh in &m.meshes {
            if std::env::var("AERUGO_DEBUG_WORLD").is_ok() {
                eprintln!("mesh {} world {:?}", mesh.name, mesh.world.to_cols_array());
            }
            match (&mesh.skeleton, mesh.skinned) {
                (Some(sk), true) => {
                    // AERUGO_SHOT_CHR_MOTION = index into the character's own motions: pose it at that motion's
                    // first frame (as the field does), and print each part's first skinning matrix.
                    let mut local = sk.default_pose.clone();
                    if let Some(k) =
                        std::env::var("AERUGO_SHOT_CHR_MOTION").ok().and_then(|v| v.parse::<usize>().ok())
                    {
                        let motions: Vec<aerugo_formats::motion::Motion> =
                            aerugo_sim::field::motion_groups(&vfs, path, 0, None)
                                .into_iter()
                                .flat_map(|g| g.motions)
                                .collect();
                        if let Some(motion) = motions.get(k) {
                            let scale = aerugo_formats::motion::fit_translation_scale(
                                &sk.names,
                                &sk.default_pose,
                                &motions[0],
                            );
                            for (i, name) in sk.names.iter().enumerate() {
                                if let Some(j) = aerugo_formats::motion::ps2_joint(name)
                                    .filter(|&j| j < motion.joints as usize)
                                {
                                    local[i] = motion.joint_at(j, 0.0).matrix(scale);
                                }
                            }
                        }
                    }
                    let world = sk.world(&local);
                    for (pi, &si) in mesh.part_segments.iter().enumerate() {
                        let seg = &m.model.meshes[mesh.source].segments[si];
                        let base = joints.len() as u32;
                        let palette = sk.palette(&world, seg);
                        if std::env::var("AERUGO_DEBUG_WORLD").is_ok()
                            && let Some(p0) = palette.first()
                        {
                            let full = place * mesh.world * Mat4::from_cols_array(p0);
                            eprintln!(
                                "part {pi} bone0 (game space, rows) {:?}",
                                full.transpose().to_cols_array()
                            );
                        }
                        joints.extend(palette);
                        draws.push(Draw {
                            mesh: mesh.mesh,
                            model: (place * mesh.world).to_cols_array(),
                            joint_base: Some(base),
                            lit: false,
                            part: Some(pi),
                            tint,
                        });
                    }
                }
                _ => draws.push(Draw {
                    mesh: mesh.mesh,
                    model: (place * mesh.world).to_cols_array(),
                    joint_base: None,
                    lit: false,
                    part: None,
                    tint,
                }),
            }
        }
    }
    let extent = (model.max - model.min).length() * 10.0;
    let (lo, hi) = (map_place().transform_point3(model.min), map_place().transform_point3(model.max));
    eprintln!("map bounds (game units) {:?} .. {:?}", lo.min(hi), lo.max(hi));
    // The map's fog as the game screen draws it at an event's start (limit 0: none until a script sets one);
    // AERUGO_SHOT_FOG = `none`, `map` (the limit the map stores), or a limit (a number, as native 803C / 255
    // would set), with an optional `,k` scaling the fog distances, for fitting to captures.
    let fog = model.scene.and_then(|s| {
        let spec = std::env::var("AERUGO_SHOT_FOG").unwrap_or_default();
        if spec == "none" {
            return None;
        }
        let mut f = assets::scene_fog(&s);
        let (limit, k) = spec.split_once(',').map_or((spec.as_str(), 1.0), |(l, k)| (l, k.parse().unwrap()));
        match limit.parse() {
            Ok(l) => f.limit = l,
            Err(_) if limit == "map" => {}
            Err(_) => f.limit = 0.0,
        }
        f.near *= k;
        f.far *= k;
        eprintln!("fog {f:?}");
        Some(f)
    });
    let size = size();
    let aspect = size.0 as f32 / size.1 as f32;
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
                    clear: model.scene.map_or([0.0, 0.0, 0.0, 1.0], |s| {
                        [s.background[0], s.background[1], s.background[2], 1.0]
                    }),
                    draws: draws.clone(),
                    joints: joints.clone(),
                    culling: std::env::var("AERUGO_SHOT_NOCULL").is_err(),
                    mirrored: false,
                    fog,
                    time: std::env::var("AERUGO_SHOT_TIME").ok().and_then(|t| t.parse().ok()).unwrap_or(0.0),
                    // Off as on the game screen; AERUGO_SHOT_CC applies the map's colour-correction table.
                    color_correction: model
                        .color_correction
                        .filter(|_| std::env::var("AERUGO_SHOT_CC").is_ok()),
                    // AERUGO_SHOT_NOGLOW leaves out the map's glow.
                    glow: model
                        .scene
                        .as_ref()
                        .filter(|_| std::env::var("AERUGO_SHOT_NOGLOW").is_err())
                        .map(assets::scene_glow),
                    character_light: character_light(model.scene.as_ref().map(assets::scene_glow), map),
                },
                size,
            )
            .unwrap();
        let name = if tag.is_empty() {
            out.to_owned()
        } else {
            format!("{}-{tag}.png", out.trim_end_matches(".png"))
        };
        let file = std::fs::File::create(name).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), size.0, size.1);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().and_then(|mut w| w.write_image_data(&rgba)).unwrap();
    }
    if validate {
        assert_eq!(aerugo_gfx::validation_error_count(), 0, "Vulkan validation errors (see stderr)");
    }
}
