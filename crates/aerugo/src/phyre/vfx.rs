//! The remaster's effect (VFX) draws on its own shaders: each frame's effect geometry (built by the sim as
//! the remaster's renderer builds it) drawn with the VFX effect its shader properties pick
//! (`shaders/vfxShader_fx.txt`), the HD texture named by each segment's PS2 texture, and the effect's
//! parameters (World, WorldView, WorldViewProjection from the scene camera; the rest from the effect's own
//! material defaults). The geometry is in the PS2 space; the remaster's space flips x and y.

use std::collections::HashMap;

use aerugo_formats::dxbc::ResourceKind;
use aerugo_formats::mesh::Material;
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::game::{
    BufferHandle, GameDraw, GameFrame, GameOp, ImageRef, IndexBinding, Stage, TargetRef, VertexBinding,
};
use aerugo_gfx::{Renderer, TextureHandle};
use aerugo_sim::ppp::vfx::Mesh;
use aerugo_vfs::Vfs;
use glam::{Mat4, Vec4};

use super::scene::SceneConstants;
use super::{ProgramKey, Programs, Value, fill, render_state};

const SHADERS: &str = "ffx_data/gamedata/ps3data/shaders";

fn push_aligned(out: &mut Vec<u8>, data: &[u8]) -> u64 {
    while !out.len().is_multiple_of(16) {
        out.push(0);
    }
    let at = out.len() as u64;
    out.extend_from_slice(data);
    at
}

fn floats<const N: usize>(v: &[[f32; N]]) -> Vec<u8> {
    v.iter().flatten().flat_map(|f| f.to_le_bytes()).collect()
}

/// The VFX effects, textures and effect defaults loaded so far (failures remembered).
#[derive(Debug, Default)]
pub struct VfxRenderer {
    table: Option<Vec<(u32, String)>>,
    textures: HashMap<String, Option<TextureHandle>>,
    materials: HashMap<String, Option<Material>>,
}

impl VfxRenderer {
    fn effect_path(&mut self, vfs: &Vfs, properties: u32) -> Option<String> {
        let table = self.table.get_or_insert_with(|| {
            vfs.read(&format!("{SHADERS}/vfxShader_fx.txt"))
                .ok()
                .map(|d| aerugo_formats::vfx_shader::parse_table(&String::from_utf8_lossy(&d)))
                .unwrap_or_default()
        });
        let file = aerugo_formats::vfx_shader::shader_for(table, properties)?;
        Some(format!("{SHADERS}/d3d11/{}", file.to_ascii_lowercase()))
    }

    fn material(&mut self, vfs: &Vfs, path: &str) -> Option<&Material> {
        self.materials
            .entry(path.to_owned())
            .or_insert_with(|| {
                vfs.read(path)
                    .ok()
                    .and_then(|d| Cluster::parse(d).ok())
                    .and_then(|c| aerugo_formats::mesh::first_material(&c))
            })
            .as_ref()
    }

    fn texture(&mut self, vfs: &Vfs, renderer: &mut Renderer, path: &str) -> Option<TextureHandle> {
        *self.textures.entry(path.to_owned()).or_insert_with(|| {
            crate::assets::load_texture(vfs, renderer, path).map_err(|e| log::debug!("vfx: {path}: {e}")).ok()
        })
    }

    /// Release the textures.
    pub fn clear(&mut self, renderer: &mut Renderer) {
        for h in std::mem::take(&mut self.textures).into_values().flatten() {
            renderer.destroy_texture(h);
        }
    }

    /// The event's 2D shapes (the credits) over the finished frame, as the remaster's 2D effect pass draws them:
    /// each piece a quad (corners ÷ 15 × height ÷ 416), coloured by its bytes ÷ 256 times the slot's ÷ 128
    /// (alpha by 1/256), placed by the slot's matrix (x × width ÷ 512 − width ÷ 2, y × height ÷ 416 −
    /// height ÷ 2; at full scale from 720 lines up, kept inside 85 % of the screen), with the event's texture
    /// (`<page>_<w>_<h>`) and the VFX effect of the piece's blend. Returns what was missing.
    #[allow(clippy::too_many_arguments)]
    pub fn draws_2d(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        programs: &mut Programs,
        draws: &[aerugo_sim::field::shapes::ShapeDraw2d],
        dir: &str,
        size: (u32, u32),
        frame: &mut GameFrame,
    ) -> Vec<String> {
        let (w, h) = (size.0 as f32, size.1 as f32);
        let k = h / 416.0;
        let mut set_target = true;
        let mut missing = Vec::new();
        for d in draws {
            let corners: Vec<[[f32; 3]; 4]> = d
                .pieces
                .iter()
                .map(|p| {
                    let c = |v: i16| f32::from(v) / 15.0 * k;
                    let (x0, y0, x1, y1) = (c(p.rect[0]), c(p.rect[1]), c(p.rect[2]), c(p.rect[3]));
                    [[x0, y0, 1.0], [x1, y0, 1.0], [x0, y1, 1.0], [x1, y1, 1.0]]
                })
                .collect();
            let mut m = Mat4::IDENTITY;
            m.x_axis.x = d.scale[0];
            m.y_axis.y = d.scale[1];
            m.w_axis.x = d.place[0] * (w / 512.0) - w * 0.5;
            m.w_axis.y = d.place[1] * k - h * 0.5;
            if size.1 > 0x2cf && d.scale == [1.0, 1.0] && !corners.is_empty() {
                let all = corners.iter().flatten();
                let lo_y = all.clone().map(|v| v[1]).fold(f32::MAX, f32::min) + m.w_axis.y;
                let hi_y = all.clone().map(|v| v[1]).fold(f32::MIN, f32::max) + m.w_axis.y;
                let lo_x = all.clone().map(|v| v[0]).fold(f32::MAX, f32::min) + m.w_axis.x;
                let hi_x = all.map(|v| v[0]).fold(f32::MIN, f32::max) + m.w_axis.x;
                let (ly, hy) = (-h * 0.85 * 0.5, h * 0.85 * 0.5);
                let (lx, hx) = (w / 512.0 * -w * 0.85 * 0.5, w / 512.0 * w * 0.85 * 0.5);
                if lo_y < ly {
                    m.w_axis.y += ly - lo_y;
                } else if hy < hi_y {
                    m.w_axis.y -= hi_y - hy;
                }
                if lo_x < lx {
                    m.w_axis.x += lx - lo_x;
                } else if hx < hi_x {
                    m.w_axis.x -= hi_x - hx;
                }
            }
            let projection = Mat4::from_cols_array(&[
                2.0 / w,
                0.0,
                0.0,
                0.0,
                0.0,
                -2.0 / h,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.5,
                1.0,
            ]);
            let texture = format!("{dir}/{}_{}_{}.dds.phyre", d.page, d.width, d.height);
            let Some(tex) = self.texture(vfs, renderer, &texture) else {
                missing.push(format!("event shape texture {texture}"));
                continue;
            };
            // Runs of pieces with one blend, in drawing order.
            let mut start = 0;
            while start < d.pieces.len() {
                let props = aerugo_sim::ppp::vfx::blend_properties(d.pieces[start].blend);
                let mut end = start + 1;
                while end < d.pieces.len()
                    && aerugo_sim::ppp::vfx::blend_properties(d.pieces[end].blend) == props
                {
                    end += 1;
                }
                let idx = start..end;
                start = end;
                let Some(path) = self.effect_path(vfs, props) else { continue };
                let key =
                    ProgramKey { cluster: path.clone(), pass: "Transparent".into(), shader: 0, index: 0 };
                let Some(p) = programs.get(vfs, renderer, &key) else { continue };
                let defaults: Vec<(String, Vec<f32>)> =
                    self.material(vfs, &path).map(|m| m.values.clone()).unwrap_or_default();
                let mut seg = aerugo_sim::ppp::vfx::Segment::default();
                for i in idx {
                    let p = &d.pieces[i];
                    let base = seg.positions.len() as u16;
                    seg.positions.extend(corners[i]);
                    seg.normals.extend([[0.0, 0.0, 1.0]; 4]);
                    let ch = |j: usize| {
                        f32::from(p.colour[j]) * 0.003_906_25 * (f32::from(d.colour[j]) * 0.0078125)
                    };
                    let a = f32::from(d.colour[3]) * 0.0078125 * (f32::from(p.colour[3]) / 256.0);
                    seg.colours.extend([[ch(0), ch(1), ch(2), a]; 4]);
                    let uv = |v: i16| f32::from(v) * 0.000_244_140_62;
                    let (u, v, uw, vh) = (uv(p.uv[0]), uv(p.uv[1]), uv(p.uv[2]), uv(p.uv[3]));
                    seg.uvs.extend([
                        [u, 1.0 - v],
                        [u + uw, 1.0 - v],
                        [u, 1.0 - v - vh],
                        [u + uw, 1.0 - v - vh],
                    ]);
                    seg.indices.extend([base, base + 2, base + 1, base + 2, base + 3, base + 1]);
                }
                let value = |name: &str| -> Option<Value> {
                    Some(match name {
                        "World" | "WorldView" => Value::Matrix(m),
                        "WorldViewProjection" => Value::Matrix(projection * m),
                        "ViewProjection" => Value::Matrix(projection),
                        _ => {
                            return defaults
                                .iter()
                                .find(|(n, _)| n == name)
                                .map(|(_, v)| Value::Floats(v.clone()));
                        }
                    })
                };
                if let Some(draw) = self.segment_draw(
                    &path,
                    &p,
                    &seg,
                    Some(tex),
                    &value,
                    [0.0, 0.0, w, h, 0.0, 1.0],
                    &mut frame.transient,
                    &mut missing,
                ) {
                    if set_target {
                        frame.ops.push(GameOp::SetTargets { colors: vec![TargetRef::Output], depth: None });
                        set_target = false;
                    }
                    frame.ops.push(GameOp::Draw(Box::new(draw)));
                }
            }
        }
        missing
    }

    /// The draws of the frame's effect geometry (`meshes`: each with its texture folder), seen with `scene`'s
    /// camera; vertex data goes into `transient`.
    #[allow(clippy::too_many_arguments)]
    pub fn draws(
        &mut self,
        vfs: &Vfs,
        renderer: &mut Renderer,
        programs: &mut Programs,
        meshes: &[(Mesh, String)],
        scene: &SceneConstants,
        viewport: [f32; 6],
        transient: &mut Vec<u8>,
        missing: &mut Vec<String>,
    ) -> Vec<GameOp> {
        let mut ops = Vec::new();
        for (mesh, dir) in meshes {
            let Some(path) = self.effect_path(vfs, mesh.properties) else {
                missing.push(format!("vfx shader for properties {:#x}", mesh.properties));
                continue;
            };
            let key = ProgramKey { cluster: path.clone(), pass: "Transparent".into(), shader: 0, index: 0 };
            let Some(p) = programs.get(vfs, renderer, &key) else { continue };
            let defaults: Vec<(String, Vec<f32>)> =
                self.material(vfs, &path).map(|m| m.values.clone()).unwrap_or_default();
            let flip = Mat4::from_diagonal(Vec4::new(-1.0, -1.0, 1.0, 1.0));
            let world = flip * Mat4::from_cols_array(&mesh.world);
            let z_matrix = mesh.z_matrix.map(|z| flip * Mat4::from_cols_array(&z));
            let (view, projection) = (scene.view, scene.projection);
            let value = |name: &str| -> Option<Value> {
                Some(match name {
                    "World" => Value::Matrix(world),
                    "WorldView" => Value::Matrix(view * world),
                    "WorldViewProjection" => Value::Matrix(projection * view * world),
                    "ViewProjection" => Value::Matrix(projection * view),
                    "zMatrix" => Value::Matrix(z_matrix?),
                    _ => {
                        return defaults
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, v)| Value::Floats(v.clone()));
                    }
                })
            };
            for seg in &mesh.segments {
                if seg.indices.is_empty() {
                    continue;
                }
                let texture = match seg.texture {
                    Some(t) if !t.is_frame_buffer() && !dir.is_empty() => {
                        match self.texture(vfs, renderer, &format!("{dir}/{}", t.file_name())) {
                            Some(h) => Some(h),
                            None => continue,
                        }
                    }
                    _ => None,
                };
                if let Some(d) =
                    self.segment_draw(&path, &p, seg, texture, &value, viewport, transient, missing)
                {
                    ops.push(GameOp::Draw(Box::new(d)));
                }
            }
        }
        ops
    }
    /// One segment's draw with `p` (the vertex streams the program takes, its constants by `value`, the
    /// texture and the effect material's samplers).
    #[allow(clippy::too_many_arguments)]
    fn segment_draw(
        &self,
        path: &str,
        p: &super::Program,
        seg: &aerugo_sim::ppp::vfx::Segment,
        texture: Option<TextureHandle>,
        value: &dyn Fn(&str) -> Option<Value>,
        viewport: [f32; 6],
        transient: &mut Vec<u8>,
        missing: &mut Vec<String>,
    ) -> Option<GameDraw> {
        let mut vertex_buffers = Vec::new();
        for e in &p.inputs {
            if vertex_buffers.iter().any(|v: &VertexBinding| v.slot == e.slot) {
                continue;
            }
            let (data, stride) = match e.semantic.to_ascii_uppercase().as_str() {
                "POSITION" => (floats(&seg.positions), 12),
                "NORMAL" => (floats(&seg.normals), 12),
                "COLOR" => (floats(&seg.colours), 16),
                "TEXCOORD" => (floats(&seg.uvs), 8),
                _ => continue,
            };
            let offset = push_aligned(transient, &data);
            vertex_buffers.push(VertexBinding {
                slot: e.slot,
                buffer: BufferHandle::TRANSIENT,
                offset,
                stride,
            });
        }
        let indices: Vec<u8> = seg.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let index_at = push_aligned(transient, &indices);
        let mut constants = Vec::new();
        let mut images = Vec::new();
        let mut samplers = Vec::new();
        for (stage, shader) in [(Stage::Vertex, &p.vs), (Stage::Pixel, &p.ps)] {
            for cb in &shader.constant_buffers {
                let Some(r) = shader.resource(&cb.name) else { continue };
                let mut lookup = |n: &str| value(n);
                constants.push((stage, r.bind_point, fill(cb, &mut lookup, missing)));
            }
            for r in &shader.resources {
                match r.kind {
                    ResourceKind::Texture => images.push((stage, r.bind_point, ImageRef::Texture(texture?))),
                    ResourceKind::Sampler => {
                        let state = self
                            .materials
                            .get(path)
                            .and_then(Option::as_ref)
                            .and_then(|m| m.samplers.iter().find(|(n, _)| *n == r.name))
                            .map(|(_, s)| super::model::d3d_sampler(s, 1.0));
                        if let Some(s) = state {
                            samplers.push((stage, r.bind_point, s));
                        }
                    }
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
            count: seg.indices.len() as u32,
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
        })
    }
}
