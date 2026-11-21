//! A field frame as the remaster draws it with its own shaders: the map and the characters into an
//! multisampled colour target and a second target (the normals the ambient occlusion reads), then resolved.
//!
//! Map instances: World = 10 × the instance's node world (measured in the original's constants). Characters:
//! World = translation to the position (in the remaster's space) × a turn of yaw − 90° about Y × diag(1, −1, −1)
//! (traced), the lit character effect's shader with three lights, `BoneTransforms` = World × the skin palette,
//! the field light set's three lights (directions towards the light, in the remaster's space) and the character
//! light constants. Opaque parts draw first, then the parts whose effect pass is transparent, in submission
//! order (the original's sorting of transparent parts is not traced yet).

use std::collections::HashMap;

use aerugo_formats::skeleton::Skeleton;
use aerugo_gfx::Renderer;
use aerugo_gfx::game::{GameFrame, GameOp, TargetDesc, TargetHandle, TargetRef};
use aerugo_vfs::Vfs;
use glam::{Mat4, Vec3};

use super::Programs;
use super::Value;
use super::model::{GameModel, Overrides, main_pass, object_values, segment_draws};
use super::scene::{SceneConstants, to_hd};
use aerugo_formats::mesh::SamplerState;
use aerugo_sim::field::face::{self, CharFace, FaceLayer};

/// The remaster's World for a character standing at `pos` (game space) facing `yaw`.
pub fn character_world(pos: [f32; 3], yaw: f32) -> Mat4 {
    Mat4::from_translation(to_hd(Vec3::from(pos)))
        * Mat4::from_rotation_y(yaw - std::f32::consts::FRAC_PI_2)
        * Mat4::from_scale(Vec3::new(1.0, -1.0, -1.0))
}

/// One character to draw.
pub struct ActorDraw<'a> {
    pub path: &'a str,
    pub world: Mat4,
    /// Local joint matrices for a mesh's skeleton (mesh index, skeleton) this frame.
    pub pose: &'a dyn Fn(usize, &Skeleton) -> Vec<[f32; 16]>,
    /// MaterialColour (colour × shade factor, alpha).
    pub material: [f32; 4],
    /// CharacterShadeColor.
    pub shade: [f32; 3],
    /// The character's face sprites and its face layer stack.
    pub face: Option<&'a CharFace>,
    pub face_layers: &'a [FaceLayer],
}

/// The face materials' texture parameters, layer by layer.
const ANIM_SAMPLERS: [&str; 6] =
    ["AnimSampler", "AnimSampler2", "AnimSampler3", "AnimSampler4", "AnimSampler5", "AnimSampler6"];

/// The texture animation manager's second sampler (constructor defaults with minFilter 5, magFilter 1 and wrap 2
/// on all three axes, which Direct3D 11 makes clamp; the border colour is not read when clamping).
const ANIMATION_SAMPLER: SamplerState = SamplerState {
    min_filter: 5,
    mag_filter: 1,
    wrap: [2, 2, 2],
    lod_bias: 0.0,
    max_anisotropy: 1.0,
    border: [0; 4],
    flags: 0,
};

/// A face material's `layer1` … `layer6`: 1 for the layers the stack fills, else 0.
fn layer_value(name: &str, filled: usize) -> Option<Value> {
    let k: usize = name.strip_prefix("layer")?.parse().ok()?;
    (1..=6).contains(&k).then(|| Value::Floats(vec![if k <= filled { 1.0 } else { 0.0 }]))
}

/// A texture the engine shows at run time (a texture animation's frame), loaded on first use.
fn runtime_texture(
    cache: &mut HashMap<String, Option<aerugo_gfx::TextureHandle>>,
    vfs: &Vfs,
    renderer: &mut Renderer,
    path: &str,
) -> Option<aerugo_gfx::TextureHandle> {
    *cache.entry(path.to_owned()).or_insert_with(|| {
        crate::assets::load_texture(vfs, renderer, path).map_err(|e| log::warn!("{path}: {e}")).ok()
    })
}

/// What a frame draws: the map (drawn ten times its file size), plain models with their own placement (in the
/// remaster's space) and characters.
#[derive(Default)]
pub struct Objects<'a> {
    pub map: Option<&'a str>,
    /// What the map's keyframes set on its display objects (by `m_objID`).
    pub map_poses: &'a [aerugo_sim::field::map_objects::ObjectPose],
    /// The map's texture animations (its materials' swap textures).
    pub texture_animations: Option<&'a aerugo_sim::field::texture_animation::TextureAnimations>,
    pub models: &'a [(&'a str, Mat4)],
    pub actors: &'a [ActorDraw<'a>],
}

/// The character light constants.
#[derive(Debug, Clone, Copy, Default)]
pub struct Lighting {
    /// Directions towards each light (game space) and colours.
    pub lights: [([f32; 3], [f32; 3]); 3],
    pub ambient: [f32; 3],
    pub shade_coeff: f32,
}

/// Field drawing state: programs, uploaded models and the frame's targets.
#[derive(Debug, Default)]
pub struct FieldRenderer {
    pub programs: Programs,
    post: super::post::PostChain,
    models: HashMap<String, Option<GameModel>>,
    targets: Option<((u32, u32, u32), [TargetHandle; 4])>,
    /// Values the shaders read that nothing supplied (logged once each).
    reported: std::collections::HashSet<String>,
    /// Texture animation frames (map swap textures, character face sprites) by archive path, as they are first
    /// shown.
    swaps: HashMap<String, Option<aerugo_gfx::TextureHandle>>,
    /// Whether an effect (by asset id) sets the `CHR_TEXTURE_ANIMATION_UV` switch.
    uv_effects: HashMap<String, bool>,
}

/// Whether a material's effect variant sets the switch `CHR_TEXTURE_ANIMATION_UV` (to anything but ""), as
/// the remaster's texture animation manager checks it; cached by effect.
fn uv_animation(
    cache: &mut HashMap<String, bool>,
    vfs: &Vfs,
    game: aerugo_formats::Game,
    effect: &str,
) -> bool {
    *cache.entry(effect.to_owned()).or_insert_with(|| {
        crate::assets::load_effect(vfs, game, effect)
            .is_some_and(|e| e.switches.iter().any(|(n, v)| n == "CHR_TEXTURE_ANIMATION_UV" && !v.is_empty()))
    })
}

impl FieldRenderer {
    fn model(&mut self, vfs: &Vfs, renderer: &mut Renderer, path: &str) -> bool {
        if !self.models.contains_key(path) {
            let m = GameModel::load(vfs, renderer, path).map_err(|e| log::warn!("{path}: {e}")).ok();
            self.models.insert(path.to_owned(), m);
        }
        self.models.get(path).is_some_and(Option::is_some)
    }

    fn targets(&mut self, renderer: &mut Renderer, size: (u32, u32), msaa: u32) -> Option<[TargetHandle; 4]> {
        let key = (size.0, size.1, msaa);
        if let Some((k, t)) = self.targets
            && k == key
        {
            return Some(t);
        }
        if let Some((_, old)) = self.targets.take() {
            for t in old {
                renderer.destroy_target(t);
            }
        }
        let mut make = |format, samples| {
            renderer
                .create_target(&TargetDesc { width: size.0, height: size.1, dxgi_format: format, samples })
                .map_err(|e| log::warn!("field target: {e:#}"))
                .ok()
        };
        let t = [make(28, msaa)?, make(28, msaa)?, make(44, msaa)?, make(28, 1)?];
        self.targets = Some((key, t));
        Some(t)
    }

    /// Release everything (the models, the targets).
    pub fn clear(&mut self, renderer: &mut Renderer) {
        self.post.clear(renderer);
        for m in std::mem::take(&mut self.models).into_values().flatten() {
            m.destroy(renderer);
        }
        if let Some((_, t)) = self.targets.take() {
            for t in t {
                renderer.destroy_target(t);
            }
        }
        for h in std::mem::take(&mut self.swaps).into_values().flatten() {
            renderer.destroy_texture(h);
        }
    }

    /// A frame of `objects` seen with `scene`'s camera: through the post chain when `post` is given, else
    /// resolved and copied to the screen. `both_sides` draws every face (a viewer option).
    #[allow(clippy::too_many_arguments)]
    pub fn frame(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        objects: &Objects<'_>,
        scene: &SceneConstants,
        light: &Lighting,
        size: (u32, u32),
        msaa: u32,
        post: Option<&super::post::PostSettings>,
        anisotropy: f32,
        both_sides: bool,
    ) -> Option<GameFrame> {
        let map = objects.map;
        let actors = objects.actors;
        let [color, normal, depth, resolved] = self.targets(renderer, size, msaa)?;
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
        let viewport = [0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0];
        let mut missing = Vec::new();
        let mut later = Vec::new();
        if let Some(path) = map
            && self.model(vfs, renderer, path)
        {
            let model = self.models[path].as_ref().expect("loaded above");
            for mesh in &model.meshes {
                let source = &model.model.meshes[mesh.source];
                let world = Mat4::from_scale(Vec3::splat(10.0)) * Mat4::from_cols_array(&source.world);
                let world = usize::try_from(source.object)
                    .ok()
                    .and_then(|o| objects.map_poses.get(o))
                    .map_or(world, |p| posed_world(world, p));
                let object = object_values(world, scene.view, scene.projection);
                let values = |name: &str| scene.value(name).or_else(|| object(name));
                for seg in &mesh.segments {
                    let fseg = &source.segments[seg.source];
                    let Some(material) = source.materials.get(fseg.material as usize) else { continue };
                    let Some((pass, transparent)) = main_pass(vfs, model.game, material) else { continue };
                    // A material naming a texture animation shows the animation's current swap texture.
                    let id = &material.tex_anim_id;
                    let swap = (!id.is_empty() && !id.contains("Mat_CTA"))
                        .then(|| objects.texture_animations.and_then(|t| t.texture(id)))
                        .flatten()
                        .and_then(|p| runtime_texture(&mut self.swaps, vfs, renderer, p));
                    let overrides = Overrides {
                        textures: swap.map(|h| ("TextureSampler", Some(h))).into_iter().collect(),
                        ..Overrides::default()
                    };
                    let draws = segment_draws(
                        vfs,
                        renderer,
                        &mut self.programs,
                        model,
                        mesh,
                        seg,
                        &pass,
                        0,
                        &values,
                        anisotropy,
                        viewport,
                        &overrides,
                        &mut missing,
                    );
                    let draws = draws.into_iter().map(|d| GameOp::Draw(Box::new(d)));
                    if transparent { later.extend(draws) } else { ops.extend(draws) }
                }
            }
        }
        for &(path, place) in objects.models {
            if !self.model(vfs, renderer, path) {
                continue;
            }
            let model = self.models[path].as_ref().expect("loaded above");
            for mesh in &model.meshes {
                let source = &model.model.meshes[mesh.source];
                let world = place * Mat4::from_cols_array(&source.world);
                let object = object_values(world, scene.view, scene.projection);
                let values = |name: &str| scene.value(name).or_else(|| object(name));
                for seg in &mesh.segments {
                    let fseg = &source.segments[seg.source];
                    let Some(material) = source.materials.get(fseg.material as usize) else { continue };
                    let Some((pass, transparent)) = main_pass(vfs, model.game, material) else { continue };
                    let overrides = Overrides::default();
                    let draws = segment_draws(
                        vfs,
                        renderer,
                        &mut self.programs,
                        model,
                        mesh,
                        seg,
                        &pass,
                        0,
                        &values,
                        anisotropy,
                        viewport,
                        &overrides,
                        &mut missing,
                    );
                    let draws = draws.into_iter().map(|d| GameOp::Draw(Box::new(d)));
                    if transparent { later.extend(draws) } else { ops.extend(draws) }
                }
            }
        }
        for a in actors {
            if !self.model(vfs, renderer, a.path) {
                continue;
            }
            let model = self.models[a.path].as_ref().expect("loaded above");
            for mesh in &model.meshes {
                let source = &model.model.meshes[mesh.source];
                let world = a.world * Mat4::from_cols_array(&source.world);
                let object = object_values(world, scene.view, scene.projection);
                let joints = mesh.skeleton.as_ref().map(|sk| sk.world(&(a.pose)(mesh.source, sk)));
                for seg in &mesh.segments {
                    let fseg = &source.segments[seg.source];
                    let Some(material) = source.materials.get(fseg.material as usize) else { continue };
                    let Some((pass, transparent)) = main_pass(vfs, model.game, material) else { continue };
                    let bones: Option<Vec<Mat4>> =
                        mesh.skeleton.as_ref().zip(joints.as_ref()).map(|(sk, j)| {
                            sk.palette(j, fseg).iter().map(|m| world * Mat4::from_cols_array(m)).collect()
                        });
                    // A face material (`m_texAnimID` "Mat_CTA…") shows the newest six entries of the face layer
                    // stack, oldest first (only the newest when its effect sets CHR_TEXTURE_ANIMATION_UV):
                    // AnimSampler k takes entry k's texture and layer k = 1; the slots past them show the empty
                    // texture with layer k = 0. Its AnimSamplerState is the manager's clamping sampler.
                    let shown = material.tex_anim_id.contains("Mat_CTA").then(|| {
                        let uv = uv_animation(&mut self.uv_effects, vfs, model.game, &material.effect);
                        face::shown_layers(a.face_layers, uv)
                    });
                    let mut overrides = Overrides::default();
                    if let Some(shown) = shown {
                        for (k, slot) in ANIM_SAMPLERS.into_iter().enumerate() {
                            let path =
                                shown.get(k).zip(a.face).and_then(|(l, f)| f.texture(l.sprite, l.frame));
                            let texture =
                                path.and_then(|p| runtime_texture(&mut self.swaps, vfs, renderer, p));
                            overrides.textures.push((slot, texture));
                        }
                        overrides.samplers.push(("AnimSamplerState", ANIMATION_SAMPLER));
                    }
                    let filled = shown.map(<[FaceLayer]>::len);
                    let values = |name: &str| {
                        scene
                            .value(name)
                            .or_else(|| object(name))
                            .or_else(|| filled.and_then(|n| layer_value(name, n)))
                            .or_else(|| character_value(name, a, light, &bones))
                    };
                    let draws = segment_draws(
                        vfs,
                        renderer,
                        &mut self.programs,
                        model,
                        mesh,
                        seg,
                        &pass,
                        3,
                        &values,
                        anisotropy,
                        viewport,
                        &overrides,
                        &mut missing,
                    );
                    let draws = draws.into_iter().map(|d| GameOp::Draw(Box::new(d)));
                    if transparent || a.material[3] < 1.0 { later.extend(draws) } else { ops.extend(draws) }
                }
            }
        }
        ops.extend(later);
        if both_sides {
            for op in &mut ops {
                if let GameOp::Draw(d) = op {
                    d.state.raster.cull = 1;
                }
            }
        }
        let game = map.or(objects.models.first().map(|m| m.0)).or(actors.first().map(|a| a.path));
        let game = game.map_or(aerugo_formats::Game::Ffx, crate::assets::game_of_path);
        match post {
            Some(post) => self.post.run(
                vfs,
                renderer,
                &mut self.programs,
                game,
                (color, normal, depth),
                size,
                msaa,
                size,
                scene,
                post,
                &mut ops,
            ),
            None => {
                if msaa > 1 {
                    ops.push(GameOp::Resolve { src: color, dst: TargetRef::Target(resolved) });
                    ops.push(GameOp::Copy { src: resolved, dst: TargetRef::Output });
                } else {
                    ops.push(GameOp::Copy { src: color, dst: TargetRef::Output });
                }
            }
        }
        for m in missing {
            if self.reported.insert(m.clone()) {
                log::info!("game shaders: nothing supplies {m}");
            }
        }
        Some(GameFrame { ops, transient: Vec::new() })
    }
}

/// Values only characters take: their lights, colours and bones.
fn character_value(
    name: &str,
    a: &ActorDraw<'_>,
    light: &Lighting,
    bones: &Option<Vec<Mat4>>,
) -> Option<Value> {
    let f = |v: &[f32]| Some(Value::Floats(v.to_vec()));
    let light_part = |i: usize, member: &str| -> Option<Value> {
        let (dir, color) = light.lights.get(i)?;
        match member {
            "m_direction" => f(&to_hd(Vec3::from(*dir)).to_array()),
            "m_colorIntensity" => f(color),
            _ => None,
        }
    };
    if let Some(rest) = name.strip_prefix("Light")
        && let Some((i, member)) = rest.split_once('.')
        && let Ok(i) = i.parse::<usize>()
    {
        return light_part(i, member);
    }
    match name {
        "BoneTransforms" => bones.clone().map(Value::Matrices),
        "MaterialColour" => f(&a.material),
        "CharacterShadeColor" => f(&a.shade),
        "CharacterAmbientLight" => f(&light.ambient),
        "CharacterShadeCoeff" => f(&[light.shade_coeff]),
        // The PS2 light matrices with no point lights: colour 1 from (0, 1, 0) (the remaster's default).
        "LgtMatVec" => f(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        "LgtMatClr" => f(&[1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        _ => None,
    }
}

/// A map part's world matrix as its display object's keyframes leave it. A rotation replaces the 3×3 part with ten
/// times the three axis rotations in the object's order (X = Rx(−a0), Y = Ry(−a1), Z = Rz(a2); order 0 ZYX, 1 ZXY,
/// 2 YXZ, 3 YZX, 4 XZY, 5 XYZ, each multiplied on the right); a position replaces the translation with
/// (−x, −y, z). Whatever was not set stays.
pub fn posed_world(world: Mat4, pose: &aerugo_sim::field::map_objects::ObjectPose) -> Mat4 {
    let mut w = world;
    if let Some((a, order)) = pose.rotation {
        let (x, y, z) =
            (Mat4::from_rotation_x(-a[0]), Mat4::from_rotation_y(-a[1]), Mat4::from_rotation_z(a[2]));
        let r = match order {
            0 => z * y * x,
            1 => z * x * y,
            2 => y * x * z,
            3 => y * z * x,
            4 => x * z * y,
            5 => x * y * z,
            _ => Mat4::IDENTITY,
        };
        let m = Mat4::from_scale(Vec3::splat(10.0)) * r;
        w = Mat4::from_cols(m.x_axis, m.y_axis, m.z_axis, w.w_axis);
    }
    if let Some(p) = pose.position {
        w.w_axis = glam::Vec4::new(-p[0], -p[1], p[2], 1.0);
    }
    w
}
