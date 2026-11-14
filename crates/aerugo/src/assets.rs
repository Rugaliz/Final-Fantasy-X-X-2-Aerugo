//! Bridges between the format readers and the renderer.

use aerugo_formats::texture::{PixelFormat, Texture};
use aerugo_gfx::{TexFormat, TextureDesc};

pub fn tex_format(f: PixelFormat) -> Option<TexFormat> {
    Some(match f {
        PixelFormat::Bc1 => TexFormat::Bc1,
        PixelFormat::Bc2 => TexFormat::Bc2,
        PixelFormat::Bc3 => TexFormat::Bc3,
        PixelFormat::Rgba8 => TexFormat::Rgba8,
        PixelFormat::Bgra8 => TexFormat::Bgra8,
        PixelFormat::R8 => TexFormat::R8,
        PixelFormat::Rg8 | PixelFormat::Rgba16F | PixelFormat::Rgba32F | PixelFormat::R32F => return None,
    })
}

/// Upload description for a texture read from a cluster (`data` is the cluster's texture data).
pub fn texture_desc<'a>(t: &'a Texture, data: &'a [u8]) -> Option<TextureDesc<'a>> {
    let format = tex_format(t.format?)?;
    let levels = t.levels.len() as u32 / t.faces.max(1);
    Some(TextureDesc {
        format,
        width: t.width,
        height: t.height,
        levels,
        layers: t.faces,
        cube: t.faces == 6,
        data: t.levels.iter().map(|r| data.get(r.clone()).unwrap_or(&[])).collect(),
        name: &t.name,
    })
}

use std::collections::HashMap;

use aerugo_formats::mesh::{self, Model, VertexFormat};
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::{MeshDesc, MeshHandle, MeshPart, Renderer, SceneVertex, TextureHandle};
use aerugo_vfs::Vfs;
use glam::{Mat4, Vec3};

/// A model cluster uploaded to the GPU: one mesh per instance, plus the textures it uses.
#[derive(Debug, Default)]
pub struct GpuModel {
    pub meshes: Vec<GpuMeshInstance>,
    pub textures: Vec<TextureHandle>,
    pub min: Vec3,
    pub max: Vec3,
    pub triangles: usize,
    pub vertices: usize,
    /// Problems that did not stop loading (missing textures, …).
    pub warnings: Vec<String>,
    pub model: Model,
}

#[derive(Debug)]
pub struct GpuMeshInstance {
    pub name: String,
    pub mesh: MeshHandle,
    pub world: Mat4,
    pub skinned: bool,
    pub parts: Vec<String>,
}

impl GpuModel {
    pub fn destroy(self, renderer: &mut Renderer) {
        for m in self.meshes {
            renderer.destroy_mesh(m.mesh);
        }
        for t in self.textures {
            renderer.destroy_texture(t);
        }
    }
}

fn read_vec<const N: usize>(
    cluster: &Cluster,
    s: Option<&mesh::VertexStream>,
    count: usize,
    default: [f32; N],
) -> Vec<[f32; N]> {
    let Some(s) = s else { return vec![default; count] };
    let f = mesh::stream_floats(cluster, s);
    let n = match s.format {
        VertexFormat::F32x2 => 2,
        VertexFormat::F32x3 => 3,
        VertexFormat::F32x4 | VertexFormat::U8x4 => 4,
    };
    (0..count)
        .map(|v| {
            let mut out = default;
            for (k, o) in out.iter_mut().enumerate().take(n) {
                if let Some(&x) = f.get(v * n + k) {
                    *o = x;
                }
            }
            out
        })
        .collect()
}

/// Load a model cluster and everything it references.
pub fn load_model(vfs: &Vfs, renderer: &mut Renderer, path: &str) -> Result<GpuModel, String> {
    let cluster = Cluster::parse(vfs.read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let model = mesh::read_model(&cluster).map_err(|e| e.to_string())?;
    let mut out = GpuModel { min: Vec3::splat(f32::MAX), max: Vec3::splat(f32::MIN), ..Default::default() };
    let mut texture_cache: HashMap<String, Option<TextureHandle>> = HashMap::new();
    for m in &model.meshes {
        let world = Mat4::from_cols_array(&m.world);
        let mut vertices: Vec<SceneVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut parts = Vec::new();
        let mut names = Vec::new();
        let mut skinned = false;
        for (si, seg) in m.segments.iter().enumerate() {
            let Some(pos) = seg.positions() else { continue };
            let count = pos.count as usize;
            let p = read_vec(&cluster, Some(pos), count, [0.0; 3]);
            let n = read_vec(&cluster, seg.normals(), count, [0.0, 1.0, 0.0]);
            let uv = read_vec(&cluster, seg.stream("ST"), count, [0.0; 2]);
            let col = read_vec(&cluster, seg.stream("Color"), count, [1.0; 4]);
            let w = read_vec(&cluster, seg.stream("SkinWeights"), count, [0.0; 4]);
            let j = read_vec(&cluster, seg.stream("SkinIndices"), count, [0.0; 4]);
            skinned |= seg.stream("SkinIndices").is_some();
            let base = vertices.len() as u32;
            for v in 0..count {
                let wp = world.transform_point3(Vec3::from(p[v]));
                out.min = out.min.min(wp);
                out.max = out.max.max(wp);
                vertices.push(SceneVertex {
                    position: p[v],
                    normal: n[v],
                    uv: uv[v],
                    color: col[v],
                    weights: w[v],
                    joints: j[v].map(|x| x as u8),
                });
            }
            let first = indices.len() as u32;
            let idx = mesh::segment_indices(&cluster, seg);
            indices.extend(idx.iter().map(|&i| base + i.min(count.saturating_sub(1) as u32)));
            let material = m.materials.get(seg.material as usize);
            let texture = match material.and_then(|mat| mat.base_texture()) {
                Some(asset) => {
                    let tex_path = mesh::texture_cluster_path(path, asset);
                    *texture_cache.entry(tex_path.clone()).or_insert_with(|| {
                        match load_texture(vfs, renderer, &tex_path) {
                            Ok(h) => {
                                out.textures.push(h);
                                Some(h)
                            }
                            Err(e) => {
                                out.warnings.push(format!("{tex_path}: {e}"));
                                None
                            }
                        }
                    })
                }
                None => None,
            };
            names.push(format!(
                "segment {si}: {} triangles, material {}{}",
                idx.len() / 3,
                seg.material,
                material.and_then(|mat| mat.base_texture()).map(|t| format!(", {t}")).unwrap_or_default()
            ));
            parts.push(MeshPart {
                first_index: first,
                index_count: idx.len() as u32,
                texture,
                alpha_cutoff: 0.3,
            });
        }
        if vertices.is_empty() {
            continue;
        }
        out.triangles += indices.len() / 3;
        out.vertices += vertices.len();
        let handle = renderer
            .create_mesh(&MeshDesc { vertices: &vertices, indices: &indices, parts })
            .map_err(|e| format!("{e:#}"))?;
        out.meshes.push(GpuMeshInstance { name: m.name.clone(), mesh: handle, world, skinned, parts: names });
    }
    if out.meshes.is_empty() {
        out.min = Vec3::splat(-1.0);
        out.max = Vec3::splat(1.0);
    }
    out.model = model;
    Ok(out)
}

pub fn load_texture(vfs: &Vfs, renderer: &mut Renderer, path: &str) -> Result<TextureHandle, String> {
    let cluster = Cluster::parse(vfs.read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let t = aerugo_formats::texture::read_textures(&cluster)
        .into_iter()
        .next()
        .ok_or("no texture")?
        .map_err(|e| e.to_string())?;
    let desc = texture_desc(&t, cluster.texture_data()).ok_or("unsupported format")?;
    renderer.create_texture(&desc).map_err(|e| format!("{e:#}"))
}
