//! Models as the remaster's renderer draws them: every vertex stream uploaded as it is stored (the effects'
//! input layouts name the stream each input reads), indices as stored, textures by asset, and per segment the
//! draws of its material's effect pass.

use std::collections::HashMap;

use aerugo_formats::effect::{self, effect_cluster_path};
use aerugo_formats::mesh::{self, Material, Model, SamplerState, Segment};
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::Renderer;
use aerugo_gfx::TextureHandle;
use aerugo_gfx::game::{BufferHandle, GameDraw, ImageRef, IndexBinding, SamplerDesc, Stage, VertexBinding};
use aerugo_vfs::Vfs;
use glam::Mat4;

use super::{Program, ProgramKey, Programs, Value, fill, render_state};

/// A vertex stream on the GPU.
#[derive(Debug, Clone)]
pub struct GpuStream {
    pub semantic: String,
    pub set: u8,
    pub buffer: BufferHandle,
    pub stride: u32,
}

#[derive(Debug, Clone)]
pub struct GpuSegment {
    pub source: usize,
    pub streams: Vec<GpuStream>,
    pub index: BufferHandle,
    pub wide: bool,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct GpuMesh {
    pub source: usize,
    pub segments: Vec<GpuSegment>,
    /// The mesh's skeleton (skinned meshes).
    pub skeleton: Option<aerugo_formats::skeleton::Skeleton>,
}

/// A model cluster uploaded for drawing with the games' shaders.
#[derive(Debug)]
pub struct GameModel {
    pub path: String,
    pub game: aerugo_formats::Game,
    pub model: Model,
    pub meshes: Vec<GpuMesh>,
    pub textures: HashMap<String, Option<TextureHandle>>,
    buffers: Vec<BufferHandle>,
}

impl GameModel {
    pub fn load(vfs: &Vfs, renderer: &mut Renderer, path: &str) -> Result<GameModel, String> {
        let cluster =
            Cluster::parse(vfs.read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let model = mesh::read_model(&cluster).map_err(|e| e.to_string())?;
        let game = crate::assets::game_of_path(path);
        let vb = cluster.vertex_buffer();
        let ib = cluster.index_buffer();
        let mut buffers = Vec::new();
        let mut meshes = Vec::new();
        let mut textures: HashMap<String, Option<TextureHandle>> = HashMap::new();
        for (mi, m) in model.meshes.iter().enumerate() {
            let mut segments = Vec::new();
            for (si, seg) in m.segments.iter().enumerate() {
                let mut streams = Vec::new();
                for s in &seg.streams {
                    let Some(data) = vb.get(s.data.clone()) else { continue };
                    let b = renderer.create_buffer(data, false).map_err(|e| format!("{e:#}"))?;
                    buffers.push(b);
                    streams.push(GpuStream {
                        semantic: s.semantic.clone(),
                        set: s.set,
                        buffer: b,
                        stride: s.stride,
                    });
                }
                let Some(idx) = ib.get(seg.indices.clone()) else { continue };
                let index = renderer.create_buffer(idx, true).map_err(|e| format!("{e:#}"))?;
                buffers.push(index);
                segments.push(GpuSegment {
                    source: si,
                    streams,
                    index,
                    wide: seg.wide_indices,
                    count: seg.index_count,
                });
            }
            for mat in &m.materials {
                for t in &mat.textures {
                    if textures.contains_key(&t.asset) {
                        continue;
                    }
                    let p = mesh::texture_cluster_path(path, &t.asset);
                    let h = crate::assets::load_texture(vfs, renderer, &p)
                        .map_err(|e| log::warn!("{p}: {e}"))
                        .ok();
                    textures.insert(t.asset.clone(), h);
                }
            }
            let skeleton = aerugo_formats::skeleton::read_skeleton(&cluster, m.mesh)
                .filter(|_| m.segments.iter().any(|s| s.stream("SkinIndices").is_some()));
            meshes.push(GpuMesh { source: mi, segments, skeleton });
        }
        Ok(GameModel { path: path.to_owned(), game, model, meshes, textures, buffers })
    }

    pub fn destroy(self, renderer: &mut Renderer) {
        for b in self.buffers {
            renderer.destroy_buffer(b);
        }
        for h in self.textures.into_values().flatten() {
            renderer.destroy_texture(h);
        }
    }
}

/// The Direct3D 11 sampler the remaster makes from a Phyre sampler state: filter from the minification and
/// magnification filters (anisotropic whenever the anisotropy setting is over 1; comparison with flag bit 6),
/// address modes 1 wrap, 3 border, 4 mirror and otherwise clamp, the LOD bias, the anisotropy (the setting's,
/// rounded, when it is over 1), comparison less-or-equal or never, the border colour from its bytes, and
/// Direct3D's default LOD range.
pub fn d3d_sampler(s: &SamplerState, anisotropy: f32) -> SamplerDesc {
    let mag = u32::from(s.mag_filter != 0) * 4;
    let mut filter = match s.min_filter {
        0 | 2 => mag,
        1 | 3 => mag + 0x10,
        4 => mag + 0x1,
        5 => mag + 0x11,
        _ => 0,
    };
    if anisotropy > 1.0 {
        filter = 0x55;
    }
    let compare = s.flags >> 6 & 1 != 0;
    if compare {
        filter |= 0x80;
    }
    let address = |w: u8| match w {
        1 => 1,
        3 => 4,
        4 => 2,
        _ => 3,
    };
    let max_aniso = if anisotropy > 1.0 { anisotropy.round() } else { s.max_anisotropy };
    SamplerDesc {
        filter,
        address: s.wrap.map(address),
        mip_lod_bias: s.lod_bias.to_bits(),
        max_anisotropy: max_aniso.round() as u32,
        comparison: if compare { 4 } else { 1 },
        border: s.border.map(|b| (f32::from(b) / 255.0).to_bits()),
        ..SamplerDesc::default()
    }
}

/// Per-draw values besides the material's: `name → value` (scene constants, world matrices, lights…).
pub type Values<'a> = dyn Fn(&str) -> Option<Value> + 'a;

/// The material's values: numbers by name.
fn material_value(m: &Material, name: &str) -> Option<Value> {
    m.value(name).map(|v| Value::Floats(v.to_vec()))
}

/// Material parameters the engine writes at run time (the texture animation managers, the shadow manager):
/// texture parameters by name (`None`: the engine's empty texture; a render target for the shadow maps) and
/// sampler parameters by name.
#[derive(Debug, Default)]
pub struct Overrides {
    pub textures: Vec<(&'static str, Option<ImageRef>)>,
    pub samplers: Vec<(&'static str, SamplerState)>,
}

/// The draws of one segment in one scene pass of its material's effect (`shader` picks the light count).
#[allow(clippy::too_many_arguments)]
pub fn segment_draws(
    vfs: &Vfs,
    renderer: &mut Renderer,
    programs: &mut Programs,
    model: &GameModel,
    mesh: &GpuMesh,
    seg: &GpuSegment,
    pass: &str,
    shader: usize,
    values: &Values<'_>,
    anisotropy: f32,
    viewport: [f32; 6],
    overrides: &Overrides,
    missing: &mut Vec<String>,
) -> Vec<GameDraw> {
    let source = &model.model.meshes[mesh.source];
    let fseg: &Segment = &source.segments[seg.source];
    let Some(material) = source.materials.get(fseg.material as usize) else { return Vec::new() };
    let Some(cluster) = effect_cluster_path(model.game, &material.effect) else { return Vec::new() };
    // Effects without one shader per light count take their only one.
    let shader = if programs.has_pass(vfs, &cluster, pass, shader, 0) { shader } else { 0 };
    let mut out = Vec::new();
    for index in 0.. {
        let key = ProgramKey { cluster: cluster.clone(), pass: pass.to_owned(), shader, index };
        let Some(p) = programs.get(vfs, renderer, &key) else { break };
        let empty = programs.empty_texture(renderer);
        out.push(draw(
            &p, model, material, fseg, seg, values, anisotropy, viewport, empty, overrides, missing,
        ));
        if !programs.has_pass(vfs, &cluster, pass, shader, index + 1) {
            break;
        }
    }
    out
}

/// The texture animation manager's parameters (AnimSampler, AnimSampler2 … AnimSampler6).
fn animation_slot(name: &str) -> bool {
    name.strip_prefix("AnimSampler").is_some_and(|n| matches!(n, "" | "2" | "3" | "4" | "5" | "6"))
}

#[allow(clippy::too_many_arguments)]
fn draw(
    p: &Program,
    model: &GameModel,
    material: &Material,
    fseg: &Segment,
    seg: &GpuSegment,
    values: &Values<'_>,
    anisotropy: f32,
    viewport: [f32; 6],
    empty: Option<aerugo_gfx::TextureHandle>,
    overrides: &Overrides,
    missing: &mut Vec<String>,
) -> GameDraw {
    let mut constants = Vec::new();
    for (stage, shader) in [(Stage::Vertex, &p.vs), (Stage::Pixel, &p.ps)] {
        for cb in &shader.constant_buffers {
            let Some(r) = shader.resource(&cb.name) else { continue };
            let mut lookup = |name: &str| values(name).or_else(|| material_value(material, name));
            constants.push((stage, r.bind_point, fill(cb, &mut lookup, missing)));
        }
    }
    let mut images = Vec::new();
    let mut samplers = Vec::new();
    for (stage, shader) in [(Stage::Vertex, &p.vs), (Stage::Pixel, &p.ps)] {
        for r in &shader.resources {
            match r.kind {
                aerugo_formats::dxbc::ResourceKind::Texture => {
                    if let Some((_, h)) = overrides.textures.iter().find(|(n, _)| *n == r.name) {
                        match h.or(empty.map(ImageRef::Texture)) {
                            Some(h) => images.push((stage, r.bind_point, h)),
                            None => missing.push(format!("texture {} (run-time)", r.name)),
                        }
                        continue;
                    }
                    let binding = material.textures.iter().find(|t| t.parameter == r.name);
                    match binding.map(|t| (t, model.textures.get(&t.asset).copied().flatten())) {
                        Some((_, Some(h))) => images.push((stage, r.bind_point, ImageRef::Texture(h))),
                        Some((t, None)) => {
                            missing.push(format!("texture {} ({} not loaded)", r.name, t.asset))
                        }
                        // An empty animation slot reads zeros (see `Programs::empty_texture`); any other empty
                        // parameter is one the engine writes at run time that the port does not supply yet (a
                        // gap: reported below).
                        None => match empty.filter(|_| animation_slot(&r.name)) {
                            Some(h) => images.push((stage, r.bind_point, ImageRef::Texture(h))),
                            None => missing.push(format!("texture {} in {}", r.name, model.path)),
                        },
                    }
                }
                aerugo_formats::dxbc::ResourceKind::Sampler => {
                    let state = overrides
                        .samplers
                        .iter()
                        .find(|(n, _)| *n == r.name)
                        .map(|(_, s)| *s)
                        .or_else(|| material.samplers.iter().find(|(n, _)| *n == r.name).map(|(_, s)| *s))
                        .or_else(|| {
                            material
                                .textures
                                .iter()
                                .find(|t| format!("{}State", t.parameter) == r.name)
                                .and_then(|t| t.sampler)
                        });
                    match state {
                        Some(s) => samplers.push((stage, r.bind_point, d3d_sampler(&s, anisotropy))),
                        None => missing.push(format!("sampler {}", r.name)),
                    }
                }
                _ => {}
            }
        }
    }
    // Vertex streams: each input slot reads the stream its layout element names, in the set the instance binds
    // to that semantic index.
    let mut vertex_buffers = Vec::new();
    for e in &p.inputs {
        if vertex_buffers.iter().any(|v: &VertexBinding| v.slot == e.slot) {
            continue;
        }
        let set = fseg.input_set(&e.stream, e.index as u8);
        let stream = seg.streams.iter().find(|s| s.semantic == e.stream && s.set == set).or_else(|| {
            let mut all = seg.streams.iter().filter(|s| s.semantic == e.stream);
            let first = all.next();
            if all.next().is_none() { first } else { None }
        });
        match stream {
            Some(s) => vertex_buffers.push(VertexBinding {
                slot: e.slot,
                buffer: s.buffer,
                offset: 0,
                stride: s.stride,
            }),
            None => missing.push(format!("stream {}", e.stream)),
        }
    }
    GameDraw {
        program: p.handle,
        state: render_state(&p.state),
        topology: 4,
        vertex_buffers,
        index: Some(IndexBinding { buffer: seg.index, offset: 0, wide: seg.wide }),
        count: seg.count,
        first: 0,
        base_vertex: 0,
        instances: 1,
        constants,
        images,
        samplers,
        viewport,
        blend_factor: [1.0; 4],
        sample_mask: u32::MAX,
        stencil_ref: p.state.stencil_ref,
    }
}

/// The scene pass a material draws in: its effect's `Opaque` or `Transparent` pass (whichever it has first).
pub fn main_pass(vfs: &Vfs, game: aerugo_formats::Game, material: &Material) -> Option<(String, bool)> {
    let e = crate::assets::load_effect(vfs, game, &material.effect)?;
    let p = e.main_pass()?;
    Some((p.kind.clone(), e.is_transparent()))
}

/// World-space values every draw of an object takes: its world matrix and the products with the camera.
pub fn object_values(world: Mat4, view: Mat4, projection: Mat4) -> impl Fn(&str) -> Option<Value> {
    move |name: &str| {
        Some(Value::Matrix(match name {
            "World" => world,
            "WorldView" => view * world,
            "WorldInverse" => world.inverse(),
            "WorldViewProjection" => projection * view * world,
            "WorldViewInverse" => (view * world).inverse(),
            _ => return None,
        }))
    }
}

/// The effect variants a cluster path has, cached (for `has_pass`).
impl Programs {
    pub fn has_pass(&mut self, vfs: &Vfs, cluster: &str, pass: &str, shader: usize, index: usize) -> bool {
        let shape = self.shapes.entry(cluster.to_owned()).or_insert_with(|| {
            vfs.read(cluster)
                .ok()
                .and_then(|d| Cluster::parse(d).ok())
                .and_then(|c| effect::read_effect(&c))
                .map(|e| {
                    e.scene_passes
                        .iter()
                        .map(|sp| (sp.kind.clone(), sp.shaders.iter().map(Vec::len).collect()))
                        .collect()
                })
                .unwrap_or_default()
        });
        shape.iter().any(|(k, counts): &(String, Vec<usize>)| {
            k == pass && counts.get(shader).is_some_and(|&n| index < n)
        })
    }
}
