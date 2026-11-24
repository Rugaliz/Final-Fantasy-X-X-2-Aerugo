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
use aerugo_gfx::game::{GameFrame, GameOp, ImageRef, TargetDesc, TargetHandle, TargetRef};
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

/// Each mesh instance's World from the model's part matrices, as the character engine hands them to the
/// remaster every frame: all instances take World when every part is skinned; otherwise each part's matrix
/// goes to the instances with a segment of that part, or to the instance numbered like the part when none has
/// one (later parts win). None: the instance is not placed by a part.
pub fn instance_worlds(
    meshes: &[aerugo_formats::mesh::Mesh],
    parts: &[Mat4],
    all_skinned: bool,
    world: Mat4,
) -> Vec<Option<Mat4>> {
    if parts.is_empty() {
        return vec![None; meshes.len()];
    }
    if all_skinned {
        return vec![Some(world); meshes.len()];
    }
    let mut out = vec![None; meshes.len()];
    for (k, m) in parts.iter().enumerate() {
        let mut hit = false;
        for (i, mesh) in meshes.iter().enumerate() {
            if mesh.segments.iter().any(|s| s.part == k as i32) {
                out[i] = Some(*m);
                hit = true;
            }
        }
        if !hit && let Some(slot) = out.get_mut(k) {
            *slot = Some(*m);
        }
    }
    out
}

/// One character to draw.
pub struct ActorDraw<'a> {
    pub path: &'a str,
    pub world: Mat4,
    /// The model's part matrices (World for a skinned part, World × its bone for a rigid one), and whether every
    /// part is skinned; empty when the model's PS2 parts are not known (each instance then takes World × its own
    /// node matrix).
    pub parts: Vec<Mat4>,
    pub all_skinned: bool,
    /// Local joint matrices for a mesh's skeleton (mesh index, skeleton) this frame.
    pub pose: &'a dyn Fn(usize, &Skeleton) -> Vec<[f32; 16]>,
    /// MaterialColour (colour × shade factor, alpha).
    pub material: [f32; 4],
    /// CharacterShadeColor.
    pub shade: [f32; 3],
    /// The character's face sprites and its face layer stack.
    pub face: Option<&'a CharFace>,
    pub face_layers: &'a [FaceLayer],
    /// What the shadow manager registers for this character when it casts this frame.
    pub caster: Option<CasterSource>,
}

/// A casting character's values for the shadow manager: its body (the grid key), model name and id, the
/// length of its root's second column × 1000, its ground height and its shade.
#[derive(Debug, Clone)]
pub struct CasterSource {
    pub key: usize,
    pub name: String,
    pub id: u16,
    pub scale: f32,
    pub ground: f32,
    pub shade: f32,
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
    /// Map groups not drawn (bit per mesh instance group).
    pub hidden_map_groups: u32,
    /// The frame's effect geometry (each with the folder of its textures).
    pub vfx: &'a [(aerugo_sim::ppp::vfx::Mesh, String)],
    /// What the shadow manager needs for a field map (none for the viewers' bare maps and models).
    pub shadow: Option<ShadowSetup>,
}

/// The shadow manager's inputs: the scene's map id, the event number, the story progress (the map table's
/// conditions) and the shadow quality setting.
#[derive(Debug, Clone, Copy)]
pub struct ShadowSetup {
    pub map_id: u32,
    pub event: u32,
    pub progress: u32,
    pub quality: u32,
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
    /// The effect draws' state.
    vfx: super::vfx::VfxRenderer,
    /// The shadow manager's maps.
    shadow: super::shadow::ShadowMaps,
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
            self.load_asset_files(vfs, renderer, path);
        }
        self.models.get(path).is_some_and(Option::is_some)
    }

    /// The files the model's asset header lists (`<dir>/<name>.ahwin32` beside its `mdl` folder: the textures,
    /// texture swaps, effect textures, shaders), loaded with the asset as the remaster's loader does: textures
    /// into the run-time texture cache, the rest read.
    fn load_asset_files(&mut self, vfs: &Vfs, renderer: &mut Renderer, model: &str) {
        let Some((dir, _)) = model.rsplit_once("/mdl/") else { return };
        let Some(name) = dir.rsplit('/').next() else { return };
        let header = format!("{dir}/{name}.ahwin32");
        let Ok(text) = vfs.read(&header) else { return };
        let (files, _) =
            aerugo_sim::field::texture_animation::asset_header(&String::from_utf8_lossy(&text), "");
        for f in files {
            if f.ends_with(".dds.phyre") {
                if runtime_texture(&mut self.swaps, vfs, renderer, &f).is_none() {
                    log::error!("GAP: {header} lists {f}, which does not load");
                }
            } else if f != model && vfs.read(&f).is_err() {
                log::error!("GAP: {header} lists {f}, which does not load");
            }
        }
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

    /// The casting characters' registration in the shadow manager's grid this frame: each one's instances
    /// (their worlds as drawn and their local boxes).
    fn register_casters(&mut self, vfs: &Vfs, renderer: &mut Renderer, actors: &[ActorDraw<'_>], size: u32) {
        self.shadow.casters.begin_frame();
        for a in actors {
            let Some(c) = &a.caster else { continue };
            if !self.model(vfs, renderer, a.path) {
                continue;
            }
            let model = self.models[a.path].as_ref().expect("loaded above");
            let placed = instance_worlds(&model.model.meshes, &a.parts, a.all_skinned, a.world);
            let instances = model
                .meshes
                .iter()
                .map(|mesh| {
                    let source = &model.model.meshes[mesh.source];
                    super::shadow::CasterInstance {
                        world: placed[mesh.source]
                            .unwrap_or_else(|| a.world * Mat4::from_cols_array(&source.world)),
                        min: source.bounds_min,
                        size: source.bounds_size,
                    }
                })
                .collect();
            let input = super::shadow::CasterInput {
                key: c.key,
                name: c.name.clone(),
                id: c.id,
                instances,
                scale: c.scale,
                ground: c.ground,
                shade: c.shade,
            };
            self.shadow.casters.register(&input, size);
        }
    }

    /// The event's 2D shapes (the credits) over a finished frame.
    pub fn draw_2d_shapes(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        draws: &[aerugo_sim::field::shapes::ShapeDraw2d],
        dir: &str,
        size: (u32, u32),
        frame: &mut GameFrame,
    ) {
        for m in self.vfx.draws_2d(vfs, renderer, &mut self.programs, draws, dir, size, frame) {
            if self.reported.insert(m.clone()) {
                log::info!("game shaders: nothing supplies {m}");
            }
        }
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
        self.vfx.clear(renderer);
        self.shadow.clear(renderer);
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
        // The shadow maps (cleared when a map loads) and what the receiving materials are given this frame: in
        // modes 2 and 3 the casting characters register in the manager's grid, the seen ones whose instances
        // lie in the cascade's view cast into the advanced map; the light matrices follow this frame's fit only
        // when that set of casters changed, else last frame's stay.
        let mut shadow_plan: Option<ShadowPlan> = None;
        let receivers = match (objects.shadow, map) {
            (Some(setup), Some(path)) => {
                let mode = super::shadow::map_shadow(setup.map_id, false, setup.event, setup.progress);
                let size = super::shadow::map_size(setup.quality);
                let mut clears = Vec::new();
                let maps = self.shadow.frame(renderer, size, path, setup.map_id, &mut clears);
                ops.splice(0..0, clears);
                match maps {
                    Some(t) => {
                        let (lvp, chr_y) = match mode.mode {
                            2 | 3 => {
                                self.register_casters(vfs, renderer, actors, size);
                                let casters = &mut self.shadow.casters;
                                casters.visibility(scene.projection * scene.view, mode.visibility_test);
                                let fit = super::shadow::light_fit(
                                    scene.view,
                                    scene.projection,
                                    scene.near,
                                    mode.near[0],
                                    mode.far[0],
                                );
                                let mut now = casters.cascade(fit.raw());
                                now.sort_unstable();
                                let chosen = match self.shadow.kept.take() {
                                    Some((kept, _)) => {
                                        let mut before = casters.cascade(kept.raw());
                                        before.sort_unstable();
                                        if before != now { fit } else { kept }
                                    }
                                    None => fit,
                                };
                                self.shadow.kept = Some((chosen, now.clone()));
                                let rects = casters.atlas(&now, chosen.light_view_projection, size);
                                let keys: Vec<usize> = now
                                    .iter()
                                    .filter_map(|&i| {
                                        casters.cells.get(i).and_then(Option::as_ref).map(|c| c.key)
                                    })
                                    .collect();
                                if setup.quality != 0 && !self.shadow.fresh {
                                    shadow_plan =
                                        Some(ShadowPlan { maps: t, size, light: chosen, rects, keys });
                                }
                                (chosen.light_view_projection, casters.main_chr_y())
                            }
                            4 => {
                                if self.reported.insert("shadow mode 4".into()) {
                                    log::error!("GAP: the two-cascade shadow fit (mode 4) is not ported");
                                }
                                (Mat4::ZERO, 0.0)
                            }
                            _ => (Mat4::ZERO, 0.0),
                        };
                        Some(super::shadow::Receivers::new(t, mode.mode, size, [lvp, Mat4::ZERO], chr_y))
                    }
                    None => None,
                }
            }
            _ => None,
        };
        let mut shadow_draws: Vec<GameOp> = Vec::new();
        let shadow_textures = |o: &mut Overrides| {
            if let Some(r) = receivers {
                o.textures.push(("MySMSampler", Some(ImageRef::Target(r.map))));
                o.textures.push(("MySMSampler2", Some(ImageRef::Target(r.map2))));
            }
        };
        let shadow_value = |name: &str| receivers.as_ref().and_then(|r| r.value(name));
        let viewport = [0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0];
        let mut missing = Vec::new();
        let mut later = Vec::new();
        if let Some(path) = map
            && self.model(vfs, renderer, path)
        {
            let model = self.models[path].as_ref().expect("loaded above");
            for mesh in &model.meshes {
                let source = &model.model.meshes[mesh.source];
                if objects.hidden_map_groups >> (source.group as u32 & 31) & 1 != 0 {
                    continue;
                }
                let world = Mat4::from_scale(Vec3::splat(10.0)) * Mat4::from_cols_array(&source.world);
                let world = usize::try_from(source.object)
                    .ok()
                    .and_then(|o| objects.map_poses.get(o))
                    .map_or(world, |p| posed_world(world, p));
                let object = object_values(world, scene.view, scene.projection);
                let values =
                    |name: &str| scene.value(name).or_else(|| object(name)).or_else(|| shadow_value(name));
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
                    let mut overrides = Overrides {
                        textures: swap
                            .map(|h| ("TextureSampler", Some(ImageRef::Texture(h))))
                            .into_iter()
                            .collect(),
                        ..Overrides::default()
                    };
                    shadow_textures(&mut overrides);
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
            let placed = instance_worlds(&model.model.meshes, &a.parts, a.all_skinned, a.world);
            for mesh in &model.meshes {
                let source = &model.model.meshes[mesh.source];
                let world =
                    placed[mesh.source].unwrap_or_else(|| a.world * Mat4::from_cols_array(&source.world));
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
                    shadow_textures(&mut overrides);
                    if let Some(shown) = shown {
                        for (k, slot) in ANIM_SAMPLERS.into_iter().enumerate() {
                            let path =
                                shown.get(k).zip(a.face).and_then(|(l, f)| f.texture(l.sprite, l.frame));
                            let texture =
                                path.and_then(|p| runtime_texture(&mut self.swaps, vfs, renderer, p));
                            overrides.textures.push((slot, texture.map(ImageRef::Texture)));
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
                            .or_else(|| shadow_value(name))
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
                    if transparent || a.material[3] < 1.0 {
                        later.extend(draws)
                    } else {
                        ops.extend(draws)
                    }
                    // A caster of the cascade: its instances' "Shadow" pass from the light, into the advanced map's
                    // inner square.
                    if let (Some(plan), Some(c)) = (&shadow_plan, &a.caster)
                        && plan.keys.contains(&c.key)
                    {
                        let lf = plan.light;
                        let lit = object_values(world, lf.view, lf.projection);
                        let values = |name: &str| {
                            lit(name)
                                .or_else(|| scene.value(name))
                                .or_else(|| character_value(name, a, light, &bones))
                        };
                        let inner = (plan.size - 64) as f32;
                        let draws = segment_draws(
                            vfs,
                            renderer,
                            &mut self.programs,
                            model,
                            mesh,
                            seg,
                            "Shadow",
                            0,
                            &values,
                            anisotropy,
                            [32.0, 32.0, inner, inner, 0.0, 1.0],
                            &Overrides::default(),
                            &mut missing,
                        );
                        shadow_draws.extend(draws.into_iter().map(|d| GameOp::Draw(Box::new(d))));
                    }
                }
            }
        }
        ops.extend(later);
        // The advanced map: cleared, the casters drawn, then blurred across (into the temporary map) and back,
        // rectangle by rectangle of the atlas (each in the inner square, 32 texels in).
        if let Some(plan) = &shadow_plan {
            let [adv, _, temp, _, _] = plan.maps;
            let s = plan.size as f32;
            let mut pre = vec![
                GameOp::ClearColor(TargetRef::Target(adv), [0.0; 4]),
                GameOp::ClearColor(TargetRef::Target(temp), [0.0; 4]),
                GameOp::SetTargets { colors: vec![TargetRef::Target(adv)], depth: None },
            ];
            pre.append(&mut shadow_draws);
            let game = aerugo_formats::Game::Ffx;
            self.post.set_value("shadowblur", "GaussianBlurBufferSize", Value::Floats(vec![s, s, 0.0, 0.0]));
            self.post.set_value("shadowblur", "OutputScale", Value::Floats(vec![1.0; 4]));
            for (from, to, technique) in
                [(adv, temp, "ShadowGaussianBlurX"), (temp, adv, "ShadowGaussianBlurY")]
            {
                for r in &plan.rects {
                    let (x, y, w, h) =
                        ((r[0] + 32) as f32, (r[1] + 32) as f32, (r[2] - r[0]) as f32, (r[3] - r[1]) as f32);
                    self.post.set_value(
                        "shadowblur",
                        "UvScaleBias",
                        Value::Floats(vec![w / s, h / s, x / s, y / s]),
                    );
                    self.post.pass_in(
                        vfs,
                        renderer,
                        &mut self.programs,
                        game,
                        "shadowblur",
                        technique,
                        "shadowblur",
                        &[("ColorBuffer", ImageRef::Target(from))],
                        TargetRef::Target(to),
                        None,
                        [x, y, w, h],
                        super::post::Shape::Quad,
                        scene,
                        anisotropy,
                        &mut pre,
                    );
                }
            }
            let at = ops
                .iter()
                .position(|o| matches!(o, GameOp::ClearDepth(..)))
                .map_or(0, |i| i.saturating_sub(1));
            ops.splice(at..at, pre);
        }
        // The effects after the scene's transparent draws.
        let mut transient = Vec::new();
        // The map's own effect textures (its `fp` folder), which the remaster loads with the map.
        let map_effect_textures = objects
            .map
            .and_then(|m| m.split_once("/mdl/").map(|(root, _)| format!("{root}/fp/tex/d3d11")))
            .unwrap_or_default();
        ops.extend(self.vfx.draws(
            vfs,
            renderer,
            &mut self.programs,
            objects.vfx,
            &map_effect_textures,
            scene,
            viewport,
            &mut transient,
            &mut missing,
        ));
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
        Some(GameFrame { ops, transient })
    }
}

/// What the advanced map's draw needs: the maps, their size, the light matrices in use, the atlas and the
/// casting bodies.
struct ShadowPlan {
    maps: [TargetHandle; 5],
    size: u32,
    light: super::shadow::LightFit,
    rects: Vec<[i32; 4]>,
    keys: Vec<usize>,
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
