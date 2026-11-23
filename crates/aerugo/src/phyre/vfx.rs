//! The remaster's effect (VFX) draws on its own shaders: each frame's effect geometry (built by the sim as
//! the remaster's renderer builds it) drawn with the VFX effect its shader properties pick
//! (`shaders/vfxShader_fx.txt`), the HD texture named by each segment's PS2 texture, and the effect's
//! parameters (World, WorldView, WorldViewProjection from the scene camera; the rest from the effect's own
//! material defaults). The geometry is in the PS2 space; the remaster's space flips x and y.

use std::collections::HashMap;

use aerugo_formats::dxbc::ResourceKind;
use aerugo_formats::mesh::Material;
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::game::{BufferHandle, GameDraw, GameOp, ImageRef, IndexBinding, Stage, VertexBinding};
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
                let mut ok = true;
                for (stage, shader) in [(Stage::Vertex, &p.vs), (Stage::Pixel, &p.ps)] {
                    for cb in &shader.constant_buffers {
                        let Some(r) = shader.resource(&cb.name) else { continue };
                        let mut lookup = |n: &str| value(n);
                        constants.push((stage, r.bind_point, fill(cb, &mut lookup, missing)));
                    }
                    for r in &shader.resources {
                        match r.kind {
                            ResourceKind::Texture => match texture {
                                Some(h) => images.push((stage, r.bind_point, ImageRef::Texture(h))),
                                None => ok = false,
                            },
                            ResourceKind::Sampler => {
                                let state = self
                                    .materials
                                    .get(&path)
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
                if !ok {
                    continue;
                }
                ops.push(GameOp::Draw(Box::new(GameDraw {
                    program: p.handle,
                    state: render_state(&p.state),
                    topology: 4,
                    vertex_buffers,
                    index: Some(IndexBinding {
                        buffer: BufferHandle::TRANSIENT,
                        offset: index_at,
                        wide: false,
                    }),
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
                })));
            }
        }
        ops
    }
}
