//! The 2D layer on the remaster's menu effects: each [`Draw`] of a [`Canvas`] becomes one draw of
//! `menudefaultshader` (textured), `menudefaultshadernt` (plain) or `optimizedfontshader` (glyphs), drawn over
//! the finished picture with the effects' own state and an orthographic projection of the 1920 × 1080 HD space.

use std::collections::HashMap;

use aerugo_formats::dxbc::ResourceKind;
use aerugo_gfx::game::{
    BufferHandle, GameDraw, GameFrame, GameOp, ImageRef, IndexBinding, SamplerDesc, Stage, TargetRef,
    VertexBinding,
};
use aerugo_gfx::{Renderer, TextureHandle};
use aerugo_sim::ui2d::{Canvas, Draw, HD_H, HD_W, Shading};
use aerugo_vfs::Vfs;
use glam::Mat4;

use super::{ProgramKey, Programs, Value, fill, render_state};

const SHADERS: &str = "ffx_data/gamedata/ps3data/shaders/d3d11";

/// The menu manager's sampler: anisotropic, clamped.
fn sampler() -> SamplerDesc {
    SamplerDesc {
        filter: 0x55,
        address: [3, 3, 3],
        mip_lod_bias: 0,
        max_anisotropy: 4,
        comparison: 1,
        border: [0; 4],
        min_lod: (-f32::MAX).to_bits(),
        max_lod: f32::MAX.to_bits(),
    }
}

/// Textures the 2D layer has loaded (failures remembered).
#[derive(Debug, Default)]
pub struct UiRenderer {
    textures: HashMap<String, Option<TextureHandle>>,
}

fn push_aligned(out: &mut Vec<u8>, data: &[u8]) -> u64 {
    while !out.len().is_multiple_of(16) {
        out.push(0);
    }
    let at = out.len() as u64;
    out.extend_from_slice(data);
    at
}

fn floats(v: impl IntoIterator<Item = f32>) -> Vec<u8> {
    v.into_iter().flat_map(f32::to_le_bytes).collect()
}

impl UiRenderer {
    fn texture(&mut self, vfs: &Vfs, renderer: &mut Renderer, path: &str) -> Option<TextureHandle> {
        if let Some(t) = self.textures.get(path) {
            return *t;
        }
        let t = match crate::assets::load_texture(vfs, renderer, path) {
            Ok(t) => Some(t),
            Err(e) => {
                log::warn!("2D layer: {path}: {e}");
                None
            }
        };
        self.textures.insert(path.to_owned(), t);
        t
    }

    /// Append the canvas's draws to `frame`, over its output (`size` in pixels). `lang` is the menu language
    /// folder suffix.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        programs: &mut Programs,
        canvas: &Canvas,
        lang: &str,
        size: (u32, u32),
        frame: &mut GameFrame,
    ) {
        let mut set_target = true;
        for d in canvas.draws.iter().filter(|d| !d.quads.is_empty()) {
            if let Some(draw) = self.one(vfs, renderer, programs, d, lang, size, frame) {
                if set_target {
                    frame.ops.push(GameOp::SetTargets { colors: vec![TargetRef::Output], depth: None });
                    set_target = false;
                }
                frame.ops.push(GameOp::Draw(Box::new(draw)));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn one(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        programs: &mut Programs,
        d: &Draw,
        lang: &str,
        size: (u32, u32),
        frame: &mut GameFrame,
    ) -> Option<GameDraw> {
        let effect = match d.shading {
            Shading::Plain => "menudefaultshadernt",
            Shading::Textured => "menudefaultshader",
            Shading::Font => "optimizedfontshader",
        };
        let key = ProgramKey {
            cluster: format!("{SHADERS}/{effect}.fx.phyre"),
            pass: "Transparent".into(),
            shader: 0,
            index: 0,
        };
        let p = programs.get(vfs, renderer, &key)?;
        let texture = match &d.image {
            Some(img) => Some(self.texture(vfs, renderer, &img.path(lang))?),
            None => None,
        };
        let verts = d.quads.iter().flatten();
        let mut vertex_buffers = Vec::new();
        for e in &p.inputs {
            if vertex_buffers.iter().any(|v: &VertexBinding| v.slot == e.slot) {
                continue;
            }
            let (data, stride) = match e.semantic.to_ascii_uppercase().as_str() {
                "POSITION" => (floats(verts.clone().flat_map(|v| [v.pos[0], v.pos[1], 1.0])), 12),
                "NORMAL" => (floats(verts.clone().flat_map(|_| [0.0, 0.0, 1.0])), 12),
                "COLOR" => (floats(verts.clone().flat_map(|v| v.color)), 16),
                "TEXCOORD" => (floats(verts.clone().flat_map(|v| v.uv)), 8),
                _ => continue,
            };
            let offset = push_aligned(&mut frame.transient, &data);
            vertex_buffers.push(VertexBinding {
                slot: e.slot,
                buffer: BufferHandle::TRANSIENT,
                offset,
                stride,
            });
        }
        let indices: Vec<u8> = (0..d.quads.len() as u16)
            .flat_map(|q| [0, 1, 3, 3, 1, 2].map(|k| q * 4 + k))
            .flat_map(u16::to_le_bytes)
            .collect();
        let index_at = push_aligned(&mut frame.transient, &indices);
        // World flips x and y; the projection maps the HD space (y down) to clip space.
        let world = Mat4::from_diagonal(glam::vec4(-1.0, -1.0, 1.0, 1.0));
        let wvp = Mat4::from_diagonal(glam::vec4(2.0 / HD_W, -2.0 / HD_H, 1.0 / 65536.0, 1.0));
        let value = |name: &str| match name {
            "World" => Some(Value::Matrix(world)),
            "WorldView" => Some(Value::Matrix(Mat4::IDENTITY)),
            "WorldViewProjection" => Some(Value::Matrix(wvp)),
            "ColorScale" | "globalColorMask" | "globalColorMaskN" => Some(Value::Floats(vec![0.0; 4])),
            _ => None,
        };
        let mut constants = Vec::new();
        let mut missing = Vec::new();
        let mut images = Vec::new();
        let mut samplers = Vec::new();
        for (stage, shader) in [(Stage::Vertex, &p.vs), (Stage::Pixel, &p.ps)] {
            for cb in &shader.constant_buffers {
                let Some(r) = shader.resource(&cb.name) else { continue };
                let mut lookup = |n: &str| value(n);
                constants.push((stage, r.bind_point, fill(cb, &mut lookup, &mut missing)));
            }
            for r in &shader.resources {
                match r.kind {
                    ResourceKind::Texture => images.push((stage, r.bind_point, ImageRef::Texture(texture?))),
                    ResourceKind::Sampler => samplers.push((stage, r.bind_point, sampler())),
                    _ => {}
                }
            }
        }
        Some(GameDraw {
            program: p.handle,
            state: render_state(&p.state),
            topology: 4,
            vertex_buffers,
            index: Some(IndexBinding { buffer: BufferHandle::TRANSIENT, offset: index_at, wide: false }),
            count: d.quads.len() as u32 * 6,
            first: 0,
            base_vertex: 0,
            instances: 1,
            constants,
            images,
            samplers,
            viewport: [0.0, 0.0, size.0 as f32, size.1 as f32, 0.0, 1.0],
            blend_factor: [1.0; 4],
            sample_mask: u32::MAX,
            stencil_ref: p.state.stencil_ref,
        })
    }
}
