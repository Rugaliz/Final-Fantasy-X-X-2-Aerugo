//! The remaster's post-processing, on its own effects, as its post-process manager runs it after a field
//! scene: resolve, HDAO (depth resolve, buffer generation, blurs, combine), glow (glow map, three blurred levels
//! down and back up, added to the scene), the unsharp mask, SMAA, brightness, and the copy to the screen.
//!
//! Each post material keeps its parameter buffer between passes and frames, as Phyre's materials do; a new one
//! starts zeroed. Every pass writes the values the manager's code writes (see the private notes for where each
//! comes from) and draws one of Phyre's fullscreen geometries. Every copy clears its target first. Distortion
//! and motion blur are event effects and are not part of this chain.

use std::collections::HashMap;

use aerugo_formats::Game;
use aerugo_formats::dxbc::ResourceKind;
use aerugo_formats::mesh::SamplerState;
use aerugo_gfx::Renderer;
use aerugo_gfx::TextureHandle;
use aerugo_gfx::game::{
    BufferHandle, GameDraw, GameOp, ImageRef, Stage, TargetDesc, TargetHandle, TargetRef, VertexBinding,
};
use aerugo_vfs::Vfs;

use super::model::d3d_sampler;
use super::scene::SceneConstants;
use super::{ProgramKey, Programs, Value, fill, render_state};

/// What the post chain does this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PostSettings {
    /// HDAO level: 0 off, 1–3 (6, 12 or 20 rings).
    pub hdao: u32,
    /// The map's glow (upper and lower threshold, gain, spread), if it has one.
    pub glow: Option<[f32; 4]>,
    pub unsharp: bool,
    /// SMAA quality 1–4 (0 off).
    pub smaa: u32,
    /// (Brightness setting − 50) / 100.
    pub brightness: f32,
    pub anisotropy: f32,
}

/// The manager's HDAO defaults (constructor): fade-out, then kernel scale, reject radius, accept radius,
/// intensity, blur distance, accept angle (the same for characters and scenery), and edge sharpness.
const HDAO_FADE: f32 = 1.5;
const HDAO: [f32; 6] = [1.0, 0.43, 0.003_12, 0.96, 0.9, 0.98];
const HDAO_EDGE: f32 = 1.0;
const HDAO_BLUR_ITERATIONS: usize = 2;
/// The unsharp mask's defaults: blur distance (pixels of its quarter-size buffer), intensity, threshold.
const USM: [f32; 3] = [1.2, 0.45, 0.04];
/// Glow levels and the factor the up passes scale their spread term by (+0x5c).
const GLOW_LEVELS: usize = 3;
const GLOW_UP_FACTOR: f32 = 1.0;

#[derive(Debug, Clone, Copy)]
struct Targets {
    size: (u32, u32, u32),
    resolved: TargetHandle,
    normal: TargetHandle,
    depth: TargetHandle,
    a: TargetHandle,
    b: TargetHandle,
    usm: TargetHandle,
    glow_x: [TargetHandle; GLOW_LEVELS],
    glow_y: [TargetHandle; GLOW_LEVELS],
    smaa_depth: TargetHandle,
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    tri_pos: BufferHandle,
    tri_uv: BufferHandle,
    quad_pos: BufferHandle,
    quad_uv: BufferHandle,
    smaa_pos: BufferHandle,
    smaa_uv: BufferHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// One triangle over the screen (copies, glow).
    Triangle,
    /// Two triangles (HDAO, unsharp mask, brightness).
    Quad,
    /// SMAA's two triangles, whose texture coordinates run the other way vertically.
    SmaaQuad,
}

/// The post chain's state: targets, Phyre's fullscreen geometry, the SMAA tables and the materials' values.
#[derive(Debug, Default)]
pub struct PostChain {
    targets: Option<Targets>,
    geometry: Option<Geometry>,
    smaa_tables: Option<(Option<TextureHandle>, Option<TextureHandle>)>,
    materials: HashMap<&'static str, HashMap<String, Value>>,
}

fn floats(v: &[f32]) -> Value {
    Value::Floats(v.to_vec())
}

/// A sampler parameter nobody set: Phyre's default sampler state (wrap mode 0, i.e. clamp; its filters only
/// matter without the anisotropy override).
fn default_sampler() -> SamplerState {
    SamplerState {
        min_filter: 5,
        mag_filter: 1,
        wrap: [0; 3],
        lod_bias: 0.0,
        max_anisotropy: 1.0,
        border: [0; 4],
        flags: 0,
    }
}

impl PostChain {
    pub fn clear(&mut self, renderer: &mut Renderer) {
        if let Some(t) = self.targets.take() {
            for h in [t.resolved, t.normal, t.depth, t.a, t.b, t.usm, t.smaa_depth]
                .into_iter()
                .chain(t.glow_x)
                .chain(t.glow_y)
            {
                renderer.destroy_target(h);
            }
        }
        if let Some(g) = self.geometry.take() {
            for b in [g.tri_pos, g.tri_uv, g.quad_pos, g.quad_uv, g.smaa_pos, g.smaa_uv] {
                renderer.destroy_buffer(b);
            }
        }
        if let Some((a, s)) = self.smaa_tables.take() {
            for h in [a, s].into_iter().flatten() {
                renderer.destroy_texture(h);
            }
        }
    }

    fn targets(&mut self, renderer: &mut Renderer, w: u32, h: u32, msaa: u32) -> Option<Targets> {
        if let Some(t) = self.targets
            && t.size == (w, h, msaa)
        {
            return Some(t);
        }
        if let Some(old) = self.targets.take() {
            for t in [old.resolved, old.normal, old.depth, old.a, old.b, old.usm, old.smaa_depth]
                .into_iter()
                .chain(old.glow_x)
                .chain(old.glow_y)
            {
                renderer.destroy_target(t);
            }
        }
        let mut make = |w: u32, h: u32, f: u32| {
            renderer
                .create_target(&TargetDesc { width: w.max(1), height: h.max(1), dxgi_format: f, samples: 1 })
                .map_err(|e| log::warn!("post target: {e:#}"))
                .ok()
        };
        // Glow levels: PLegacyGlowGPUBase::initialize(w·2, h·2) >> 2 for the first, halving after.
        let level = |i: u32| (w / (2 << i), h / (2 << i));
        let mut gx = Vec::new();
        let mut gy = Vec::new();
        for i in 0..GLOW_LEVELS as u32 {
            let (lw, lh) = level(i);
            gx.push(make(lw, lh, 28)?);
            gy.push(make(lw, lh, 28)?);
        }
        let t = Targets {
            size: (w, h, msaa),
            resolved: make(w, h, 28)?,
            normal: make(w, h, 28)?,
            depth: make(w, h, 41)?,
            a: make(w, h, 28)?,
            b: make(w, h, 28)?,
            usm: make(w, h, 28)?,
            glow_x: [gx[0], gx[1], gx[2]],
            glow_y: [gy[0], gy[1], gy[2]],
            smaa_depth: make(w, h, 44)?,
        };
        self.targets = Some(t);
        Some(t)
    }

    /// Phyre's fullscreen geometry, as the original's vertex and index buffers hold it.
    fn geometry(&mut self, renderer: &mut Renderer) -> Option<Geometry> {
        if let Some(g) = self.geometry {
            return Some(g);
        }
        let bytes = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
        let mut buf = |data: Vec<u8>, index: bool| renderer.create_buffer(&data, index).ok();
        let g = Geometry {
            tri_pos: buf(bytes(&[-1.0, -1.0, 1.0, 3.0, -1.0, 1.0, -1.0, 3.0, 1.0]), false)?,
            tri_uv: buf(bytes(&[0.0, 0.0, 2.0, 0.0, 0.0, 2.0]), false)?,
            quad_pos: buf(
                bytes(&[
                    -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, 1.0, 1.0, 1.0,
                    -1.0, 1.0,
                ]),
                false,
            )?,
            quad_uv: buf(bytes(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0]), false)?,
            smaa_pos: buf(
                bytes(&[
                    -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, 1.0, -1.0, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
                    -1.0, 1.0,
                ]),
                false,
            )?,
            smaa_uv: buf(bytes(&[0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0]), false)?,
        };
        self.geometry = Some(g);
        Some(g)
    }

    fn set(&mut self, material: &'static str, name: &str, v: Value) {
        self.materials.entry(material).or_default().insert(name.to_owned(), v);
    }

    /// One pass: `material`'s values (with `textures` by parameter name) drawn with an effect pass into `target`.
    #[allow(clippy::too_many_arguments)]
    fn pass(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        programs: &mut Programs,
        game: Game,
        effect: &str,
        pass: &str,
        material: &'static str,
        textures: &[(&str, ImageRef)],
        target: TargetRef,
        depth: Option<TargetHandle>,
        viewport: (u32, u32),
        shape: Shape,
        scene: &SceneConstants,
        anisotropy: f32,
        ops: &mut Vec<GameOp>,
    ) {
        let root = match game {
            Game::Ffx => "ffx_data",
            Game::Ffx2 => "ffx-2_data",
        };
        let key = ProgramKey {
            cluster: format!("{root}/gamedata/ps3data/shaders/d3d11/{effect}.fx.phyre"),
            pass: pass.to_owned(),
            shader: 0,
            index: 0,
        };
        let Some(p) = programs.get(vfs, renderer, &key) else { return };
        let Some(g) = self.geometry(renderer) else { return };
        let values = self.materials.entry(material).or_default().clone();
        let mut constants = Vec::new();
        let mut unused = Vec::new();
        for (stage, shader) in [(Stage::Vertex, &p.vs), (Stage::Pixel, &p.ps)] {
            for cb in &shader.constant_buffers {
                let Some(r) = shader.resource(&cb.name) else { continue };
                let mut lookup = |name: &str| values.get(name).cloned().or_else(|| scene.value(name));
                constants.push((stage, r.bind_point, fill(cb, &mut lookup, &mut unused)));
            }
        }
        let mut images = Vec::new();
        let mut samplers = Vec::new();
        for (stage, shader) in [(Stage::Vertex, &p.vs), (Stage::Pixel, &p.ps)] {
            for r in &shader.resources {
                match r.kind {
                    ResourceKind::Texture => {
                        if let Some((_, img)) = textures.iter().find(|(n, _)| *n == r.name) {
                            images.push((stage, r.bind_point, *img));
                        }
                    }
                    ResourceKind::Sampler => {
                        samplers.push((stage, r.bind_point, d3d_sampler(&default_sampler(), anisotropy)));
                    }
                    _ => {}
                }
            }
        }
        let (pos, uv, count) = match shape {
            Shape::Triangle => (g.tri_pos, g.tri_uv, 3),
            Shape::Quad => (g.quad_pos, g.quad_uv, 6),
            Shape::SmaaQuad => (g.smaa_pos, g.smaa_uv, 6),
        };
        let mut vertex_buffers = Vec::new();
        for e in &p.inputs {
            let (buffer, stride) =
                if e.semantic.eq_ignore_ascii_case("TEXCOORD") { (uv, 8) } else { (pos, 12) };
            if !vertex_buffers.iter().any(|v: &VertexBinding| v.slot == e.slot) {
                vertex_buffers.push(VertexBinding { slot: e.slot, buffer, offset: 0, stride });
            }
        }
        ops.push(GameOp::SetTargets { colors: vec![target], depth });
        ops.push(GameOp::Draw(Box::new(GameDraw {
            program: p.handle,
            state: render_state(&p.state),
            topology: 4,
            vertex_buffers,
            // Phyre's fullscreen draws are not indexed.
            index: None,
            count,
            first: 0,
            base_vertex: 0,
            instances: 1,
            constants,
            images,
            samplers,
            viewport: [0.0, 0.0, viewport.0 as f32, viewport.1 as f32, 0.0, 1.0],
            blend_factor: [1.0; 4],
            sample_mask: u32::MAX,
            stencil_ref: p.state.stencil_ref,
        })));
    }

    /// The manager's copy: ColorBuffer and UvScaleBias set, the target cleared, CopyBuffer drawn.
    #[allow(clippy::too_many_arguments)]
    fn copy(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        programs: &mut Programs,
        game: Game,
        src: ImageRef,
        dst: TargetRef,
        size: (u32, u32),
        scene: &SceneConstants,
        anisotropy: f32,
        ops: &mut Vec<GameOp>,
    ) {
        self.set("copy", "UvScaleBias", floats(&[1.0, 1.0, 0.0, 0.0]));
        ops.push(GameOp::ClearColor(dst, [0.0; 4]));
        self.pass(
            vfs,
            renderer,
            programs,
            game,
            "copybuffer",
            "CopyBuffer",
            "copy",
            &[("ColorBuffer", src)],
            dst,
            None,
            size,
            Shape::Triangle,
            scene,
            anisotropy,
            ops,
        );
    }

    /// The chain after the scene: `color`, `normal` and `depth` are the scene's multisampled targets.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        programs: &mut Programs,
        game: Game,
        scene_targets: (TargetHandle, TargetHandle, TargetHandle),
        size: (u32, u32),
        msaa: u32,
        output: (u32, u32),
        scene: &SceneConstants,
        s: &PostSettings,
        ops: &mut Vec<GameOp>,
    ) {
        let (w, h) = size;
        let Some(t) = self.targets(renderer, w, h, msaa) else { return };
        let (color, normal_ms, depth_ms) = scene_targets;
        let full = (w, h);
        let an = s.anisotropy;
        let m = TargetRef::Target(color);
        let tex = ImageRef::Target;
        // Without multisampling the scene targets are copied instead of resolved.
        let resolve = |src: TargetHandle, dst: TargetHandle| {
            if msaa > 1 {
                GameOp::Resolve { src, dst: TargetRef::Target(dst) }
            } else {
                GameOp::Copy { src, dst: TargetRef::Target(dst) }
            }
        };
        ops.push(resolve(color, t.resolved));
        ops.push(resolve(normal_ms, t.normal));
        // HDAO, after the depth resolve (only with 2, 4 or 8 samples).
        if s.hdao > 0 && matches!(msaa, 2 | 4 | 8) {
            let depth_name = format!("DepthBuffer{msaa}");
            let depth_pass = format!("ResolveDepthBuffer{msaa}");
            self.set("copy", "UvScaleBias", floats(&[1.0, 1.0, 0.0, 0.0]));
            self.pass(
                vfs,
                renderer,
                programs,
                game,
                "copybuffer",
                &depth_pass,
                "copy",
                &[(&depth_name, tex(depth_ms))],
                TargetRef::Target(t.depth),
                None,
                full,
                Shape::Triangle,
                scene,
                an,
                ops,
            );
            let rings = match s.hdao {
                1 => 6,
                2 => 12,
                _ => 20,
            };
            self.set("hdao", "g_numRing", Value::Words(vec![rings]));
            for suffix in ["Chr", "Sce"] {
                for (name, value) in [
                    ("g_kernelScale", HDAO[0]),
                    ("g_rejectRadius", HDAO[1]),
                    ("g_acceptRadius", HDAO[2]),
                    ("g_intensity", HDAO[3]),
                    ("g_blurDistance", HDAO[4]),
                    ("g_acceptAngle", HDAO[5]),
                ] {
                    self.set("hdao", &format!("{name}{suffix}"), floats(&[value]));
                }
            }
            self.set("hdao", "g_fadeoutScale", floats(&[HDAO_FADE]));
            self.set("hdao", "g_widthSize", floats(&[w as f32]));
            self.set("hdao", "g_heightSize", floats(&[h as f32]));
            let p = scene.projection;
            let (p00, p11, p02, p12) = (p.col(0).x, p.col(1).y, p.col(2).x, p.col(2).y);
            self.set(
                "hdao",
                "g_ssao_projInfo",
                floats(&[
                    -2.0 / (w as f32 * p00),
                    -2.0 / (h as f32 * p11),
                    (1.0 - p02) / p00,
                    (1.0 + p12) / p11,
                ]),
            );
            self.set("hdao", "g_ssao_edgeSharpness", floats(&[HDAO_EDGE]));
            let quad = Shape::Quad;
            self.pass(
                vfs,
                renderer,
                programs,
                game,
                "hdao",
                "HDAO_BufferGen",
                "hdao",
                &[("g_depthTex", tex(t.depth)), ("g_normalTex", tex(t.normal))],
                TargetRef::Target(t.a),
                None,
                full,
                quad,
                scene,
                an,
                ops,
            );
            for _ in 0..HDAO_BLUR_ITERATIONS {
                for (axis, src, dst) in [([1.0, 0.0], t.a, t.b), ([0.0, 1.0], t.b, t.a)] {
                    self.set(
                        "hdao",
                        "g_ssao_mainTex_TexelSize",
                        floats(&[1.0 / w as f32, 1.0 / h as f32, w as f32, h as f32]),
                    );
                    self.set("hdao", "g_ssao_axis", floats(&axis));
                    self.pass(
                        vfs,
                        renderer,
                        programs,
                        game,
                        "hdao",
                        "HDAO_Blur",
                        "hdao",
                        &[("g_mainTex", tex(src)), ("g_normalTex", tex(t.normal))],
                        TargetRef::Target(dst),
                        None,
                        full,
                        quad,
                        scene,
                        an,
                        ops,
                    );
                }
            }
            self.pass(
                vfs,
                renderer,
                programs,
                game,
                "hdao",
                "HDAO_Combine",
                "hdao",
                &[("g_aoTex", tex(t.a)), ("g_mainTex", tex(t.resolved))],
                TargetRef::Target(t.b),
                None,
                full,
                quad,
                scene,
                an,
                ops,
            );
            self.copy(vfs, renderer, programs, game, tex(t.b), m, full, scene, an, ops);
            ops.push(resolve(color, t.resolved));
        }
        // Glow (the glow map, then the blur loops).
        if let Some([upper, lower, gain, spread]) = s.glow {
            self.set("glowmap", "UvScaleBias", floats(&[1.0, 1.0, 0.0, 0.0]));
            self.set("glowmap", "FullGlowThreshold", floats(&[upper]));
            self.set("glowmap", "NoGlowThreshold", floats(&[lower]));
            self.pass(
                vfs,
                renderer,
                programs,
                game,
                "phyreglow",
                "GenerateGlowMap",
                "glowmap",
                &[("ColorBuffer", tex(t.resolved))],
                TargetRef::Target(t.b),
                None,
                full,
                Shape::Triangle,
                scene,
                an,
                ops,
            );
            let level = |i: usize| (w / (2 << i), h / (2 << i));
            self.set("glow", "GlowGain", floats(&[gain]));
            // The loops write the level buffer's UvScaleBias (1, 1, 0, 0) with each blur's texture.
            self.set("glow", "UvScaleBias", floats(&[1.0, 1.0, 0.0, 0.0]));
            for i in 0..GLOW_LEVELS {
                let (lw, lh) = level(i);
                let src = if i == 0 { t.b } else { t.glow_y[i - 1] };
                let k = (i + 1) as f32 * spread + 1.0;
                self.set("glow", "GaussianBlurBufferSize", floats(&[lw as f32, lh as f32, 0.0, 0.0]));
                self.set("glow", "GaussianOutputScale", floats(&[k, k, k, 1.0]));
                for (pass, src, dst) in [
                    ("RenderGaussianBlurX", src, t.glow_x[i]),
                    ("RenderGaussianBlurY", t.glow_x[i], t.glow_y[i]),
                ] {
                    self.pass(
                        vfs,
                        renderer,
                        programs,
                        game,
                        "phyreglow",
                        pass,
                        "glow",
                        &[("GlowBuffer", tex(src))],
                        TargetRef::Target(dst),
                        None,
                        (lw, lh),
                        Shape::Triangle,
                        scene,
                        an,
                        ops,
                    );
                }
            }
            for i in (0..GLOW_LEVELS).rev() {
                let (lw, lh) = level(i);
                self.set("glow", "GaussianBlurBufferSize", floats(&[lw as f32, lh as f32, 0.0, 0.0]));
                if i == 0 {
                    self.set("glow", "GaussianOutputScale", floats(&[1.0; 4]));
                }
                self.pass(
                    vfs,
                    renderer,
                    programs,
                    game,
                    "phyreglow",
                    "RenderGaussianBlurX",
                    "glow",
                    &[("GlowBuffer", tex(t.glow_y[i]))],
                    TargetRef::Target(t.glow_x[i]),
                    None,
                    (lw, lh),
                    Shape::Triangle,
                    scene,
                    an,
                    ops,
                );
                let k = if i == 0 { 1.0 } else { GLOW_UP_FACTOR / (2.0 * spread + 1.0) };
                self.set("glow", "GaussianOutputScale", floats(&[k, k, k, 1.0]));
                let (dst, vp) = if i == 0 { (m, full) } else { (TargetRef::Target(t.glow_y[i]), (lw, lh)) };
                self.pass(
                    vfs,
                    renderer,
                    programs,
                    game,
                    "phyreglow",
                    "RenderGaussianBlurYAdditive",
                    "glow",
                    &[("GlowBuffer", tex(t.glow_x[i]))],
                    dst,
                    None,
                    vp,
                    Shape::Triangle,
                    scene,
                    an,
                    ops,
                );
            }
            ops.push(resolve(color, t.resolved));
        }
        // Unsharp mask: the picture copied down to a quarter, blurred both ways, applied.
        if s.unsharp {
            let (q1, q2) = ((w / 2, h / 2), (w / 4, h / 4));
            self.copy(
                vfs,
                renderer,
                programs,
                game,
                tex(t.resolved),
                TargetRef::Target(t.a),
                full,
                scene,
                an,
                ops,
            );
            self.copy(
                vfs,
                renderer,
                programs,
                game,
                tex(t.a),
                TargetRef::Target(t.glow_x[0]),
                q1,
                scene,
                an,
                ops,
            );
            self.copy(
                vfs,
                renderer,
                programs,
                game,
                tex(t.glow_x[0]),
                TargetRef::Target(t.glow_x[1]),
                q2,
                scene,
                an,
                ops,
            );
            self.set("usm", "intensity", floats(&[USM[1]]));
            self.set("usm", "threshhold", floats(&[USM[2]]));
            for (offsets, src, dst) in [
                ([0.0, USM[0] / q2.1 as f32, 0.0, 0.0], t.glow_x[1], t.glow_y[1]),
                ([USM[0] / q2.0 as f32, 0.0, 0.0, 0.0], t.glow_y[1], t.glow_x[1]),
            ] {
                self.set("usm", "offsets", floats(&offsets));
                self.pass(
                    vfs,
                    renderer,
                    programs,
                    game,
                    "unsharpmask",
                    "USM_Blur",
                    "usm",
                    &[("blur_MainTex", tex(src))],
                    TargetRef::Target(dst),
                    None,
                    q2,
                    Shape::Quad,
                    scene,
                    an,
                    ops,
                );
            }
            self.pass(
                vfs,
                renderer,
                programs,
                game,
                "unsharpmask",
                "USM_Apply",
                "usm",
                &[("apply_MainTex", tex(t.resolved)), ("apply_MainTexBlurred", tex(t.glow_x[1]))],
                TargetRef::Target(t.usm),
                None,
                full,
                Shape::Quad,
                scene,
                an,
                ops,
            );
            self.copy(vfs, renderer, programs, game, tex(t.usm), m, full, scene, an, ops);
            ops.push(resolve(color, t.resolved));
        }
        // SMAA (copyDepthBuffer 696af0 with the quality setting at +0x1231c).
        if s.smaa > 0 {
            if self.smaa_tables.is_none() {
                let mut load = |p: &str| {
                    crate::assets::load_texture(vfs, renderer, p).map_err(|e| log::warn!("{p}: {e}")).ok()
                };
                let area = load("ffx_data/gamedata/ps3data/shaders/tex/d3d11/areatexdx10.dds.phyre");
                let search = load("ffx_data/gamedata/ps3data/shaders/tex/d3d11/searchtex.dds.phyre");
                self.smaa_tables = Some((area, search));
            }
            let (threshold, steps, diag, corner) = match s.smaa {
                1 => (0.15, 4, 0, 0),
                2 => (0.1, 8, 0, 0),
                3 => (0.1, 16, 8, 25),
                _ => (0.05, 32, 16, 25),
            };
            self.set(
                "smaa",
                "SMAA_RT_METRICS",
                floats(&[1.0 / w as f32, 1.0 / h as f32, w as f32, h as f32]),
            );
            self.set("smaa", "SMAA_THRESHOLD", floats(&[threshold]));
            self.set("smaa", "SMAA_MAX_SEARCH_STEPS", Value::Words(vec![steps]));
            self.set("smaa", "SMAA_MAX_SEARCH_STEPS_DIAG", Value::Words(vec![diag]));
            self.set("smaa", "SMAA_CORNER_ROUNDING", Value::Words(vec![corner]));
            let (area, search) = self.smaa_tables.unwrap_or_default();
            let tables: Vec<(&str, ImageRef)> = [("areaTex", area), ("searchTex", search)]
                .into_iter()
                .filter_map(|(n, h)| Some((n, ImageRef::Texture(h?))))
                .collect();
            ops.push(GameOp::ClearColor(TargetRef::Target(t.b), [0.0; 4]));
            ops.push(GameOp::ClearDepth(t.smaa_depth, 1.0, 0, 3));
            let with = |base: &[(&'static str, ImageRef)]| -> Vec<(&'static str, ImageRef)> {
                base.iter().copied().chain(tables.iter().copied()).collect()
            };
            let edge = with(&[("colorTex", tex(t.resolved))]);
            self.pass(
                vfs,
                renderer,
                programs,
                game,
                "smaa",
                "ColorEdgeDetection",
                "smaa",
                &edge,
                TargetRef::Target(t.b),
                Some(t.smaa_depth),
                full,
                Shape::SmaaQuad,
                scene,
                an,
                ops,
            );
            ops.push(GameOp::ClearColor(TargetRef::Target(t.a), [0.0; 4]));
            let weights = with(&[("edgesTex", tex(t.b))]);
            self.pass(
                vfs,
                renderer,
                programs,
                game,
                "smaa",
                "BlendingWeightCalculation",
                "smaa",
                &weights,
                TargetRef::Target(t.a),
                Some(t.smaa_depth),
                full,
                Shape::SmaaQuad,
                scene,
                an,
                ops,
            );
            let blend = with(&[("colorTex", tex(t.resolved)), ("blendTex", tex(t.a))]);
            self.pass(
                vfs,
                renderer,
                programs,
                game,
                "smaa",
                "NeighborhoodBlending",
                "smaa",
                &blend,
                TargetRef::Target(t.b),
                None,
                full,
                Shape::SmaaQuad,
                scene,
                an,
                ops,
            );
            self.copy(vfs, renderer, programs, game, tex(t.b), m, full, scene, an, ops);
            ops.push(resolve(color, t.resolved));
        }
        // Brightness.
        self.set("bright", "g_brightPara", floats(&[s.brightness]));
        self.pass(
            vfs,
            renderer,
            programs,
            game,
            "brightnessshader",
            "BrightBlend",
            "bright",
            &[("g_BrightTex", tex(t.resolved))],
            TargetRef::Target(t.b),
            None,
            full,
            Shape::Quad,
            scene,
            an,
            ops,
        );
        self.copy(vfs, renderer, programs, game, tex(t.b), m, full, scene, an, ops);
        ops.push(resolve(color, t.resolved));
        // To the screen.
        self.set("copy", "UvScaleBias", floats(&[1.0, 1.0, 0.0, 0.0]));
        self.pass(
            vfs,
            renderer,
            programs,
            game,
            "copybuffer",
            "CopyBuffer",
            "copy",
            &[("ColorBuffer", tex(t.resolved))],
            TargetRef::Output,
            None,
            output,
            Shape::Triangle,
            scene,
            an,
            ops,
        );
    }
}
