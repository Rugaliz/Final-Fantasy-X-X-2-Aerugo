//! Meshes in Phyre model clusters (`*.dae.phyre`): instances, segments, vertex streams, indices and materials.
//!
//! Every vertex data block holds one stream; segments are triangle lists with 16-bit (rarely 32-bit) indices.
//! Vertex positions of skinned meshes are in bind pose, so a mesh drawn without skinning shows its bind pose.

use std::ops::Range;

use crate::phyre::{Cluster, ObjectId, Pointer};

/// Component layout of a vertex stream (`PVertexStream::m_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VertexFormat {
    F32x2,
    F32x3,
    F32x4,
    U8x4,
}

impl VertexFormat {
    pub fn from_type(t: u32) -> Option<VertexFormat> {
        Some(match t {
            1 => VertexFormat::F32x2,
            2 => VertexFormat::F32x3,
            3 => VertexFormat::F32x4,
            19 => VertexFormat::U8x4,
            _ => return None,
        })
    }

    pub fn size(self) -> usize {
        match self {
            VertexFormat::F32x2 => 8,
            VertexFormat::F32x3 => 12,
            VertexFormat::F32x4 => 16,
            VertexFormat::U8x4 => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexStream {
    /// Semantic name: `Vertex`, `Normal`, `ST`, `Color`, `Tangent`, `Binormal`, `Skinnable*`, `SkinWeights`,
    /// `SkinIndices`.
    pub semantic: String,
    pub format: VertexFormat,
    pub stride: u32,
    pub count: u32,
    /// Byte range in [`Cluster::vertex_buffer`].
    pub data: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Index into the mesh's materials.
    pub material: u32,
    pub streams: Vec<VertexStream>,
    pub index_count: u32,
    pub wide_indices: bool,
    /// Byte range in [`Cluster::index_buffer`].
    pub indices: Range<usize>,
    pub max_index: u32,
    /// For skinned segments: skin index → (hierarchy matrix, skeleton matrix).
    pub skin_bones: Vec<(u16, u16)>,
}

impl Segment {
    pub fn stream(&self, semantic: &str) -> Option<&VertexStream> {
        self.streams.iter().find(|s| s.semantic == semantic)
    }

    /// Position stream (`Vertex` or `SkinnableVertex`).
    pub fn positions(&self) -> Option<&VertexStream> {
        self.stream("Vertex").or_else(|| self.stream("SkinnableVertex"))
    }

    pub fn normals(&self) -> Option<&VertexStream> {
        self.stream("Normal").or_else(|| self.stream("SkinnableNormal"))
    }
}

/// A texture bound to a material parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureBinding {
    /// Shader parameter name (`TextureSampler`, `NormalMapSampler`, …).
    pub parameter: String,
    /// Asset id of the texture (`PS3Data/…/name.dds`), see [`texture_cluster_path`].
    pub asset: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Material {
    /// Effect asset id (`PS3Data/Shaders/<name>.fx#<variant>`).
    pub effect: String,
    pub textures: Vec<TextureBinding>,
}

impl Material {
    /// The base colour texture, if the material has one.
    pub fn base_texture(&self) -> Option<&str> {
        let pick = |p: &str| self.textures.iter().find(|t| t.parameter == p).map(|t| t.asset.as_str());
        pick("TextureSampler")
            .or_else(|| pick("DiffuseMapSampler"))
            .or_else(|| self.textures.first().map(|t| t.asset.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    pub name: String,
    pub instance: ObjectId,
    /// The `PMesh` object (skeleton data lives there).
    pub mesh: ObjectId,
    /// Column-major 4×4 transform of the instance, from the node that carries it (identity when none does).
    pub world: [f32; 16],
    pub segments: Vec<Segment>,
    pub materials: Vec<Material>,
    pub bounds_min: [f32; 3],
    pub bounds_size: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Model {
    pub meshes: Vec<Mesh>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MeshError {
    #[error("segment {segment}: stream {semantic} has unknown type {kind}")]
    UnknownStreamType { segment: usize, semantic: String, kind: u32 },
    #[error("segment {0}: data lies outside the buffers")]
    OutOfRange(usize),
    #[error("segment {0}: primitive type {1} is not a triangle list")]
    Primitive(usize, u32),
}

pub const IDENTITY: [f32; 16] =
    [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
const TRIANGLE_LIST: u32 = 2;
const INDEX_U32: u32 = 8;
const TEXTURE_PARAMETER: u32 = 66;

/// Archive path of the texture cluster for a texture asset id (`PS3Data/chr/x/tex/a.dds` →
/// `<game>_data/gamedata/ps3data/chr/x/tex/d3d11/a.dds.phyre`). `model_path` is the model's archive path, used
/// for the game folder.
pub fn texture_cluster_path(model_path: &str, asset: &str) -> String {
    let root = model_path.split("/gamedata/").next().unwrap_or("ffx_data");
    let rel = asset.trim_start_matches('/').to_ascii_lowercase();
    let rel = rel.strip_prefix("ps3data/").unwrap_or(&rel);
    let (dir, file) = rel.rsplit_once('/').unwrap_or(("", rel));
    let file = file.split('#').next().unwrap_or(file);
    if dir.is_empty() {
        format!("{root}/gamedata/ps3data/d3d11/{file}.phyre")
    } else {
        format!("{root}/gamedata/ps3data/{dir}/d3d11/{file}.phyre")
    }
}

/// Column-major product a × b.
pub fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    out
}

fn local_matrix(cluster: &Cluster, node: ObjectId) -> [f32; 16] {
    match cluster.floats(node, "m_localMatrix.m_elements", 16) {
        Some(f) if f.iter().all(|x| x.is_finite()) => f.try_into().unwrap_or(IDENTITY),
        _ => IDENTITY,
    }
}

/// World transform of a node: its local matrix times its parents', root first.
pub fn node_world(cluster: &Cluster, node: ObjectId) -> [f32; 16] {
    let mut chain = vec![node];
    while let Some(p) = cluster.target(*chain.last().expect("non-empty"), "m_parent") {
        if chain.len() > 256 || chain.contains(&p) {
            break;
        }
        chain.push(p);
    }
    chain.iter().rev().fold(IDENTITY, |acc, &n| mul(&acc, &local_matrix(cluster, n)))
}

/// The instance's transform: the node that shares its world matrix object, if any.
fn instance_world(cluster: &Cluster, inst: ObjectId) -> [f32; 16] {
    let Some(wm) = cluster.target(inst, "m_localToWorldMatrix") else { return IDENTITY };
    cluster
        .objects("PNode")
        .into_iter()
        .find(|&n| cluster.target(n, "m_worldMatrix") == Some(wm))
        .map(|n| node_world(cluster, n))
        .unwrap_or(IDENTITY)
}

fn material(cluster: &Cluster, mat: ObjectId) -> Material {
    let effect = match cluster.pointer(mat, "m_effectVariant") {
        Some(Pointer::User(u)) => cluster.import(u).map(|(id, _)| id).unwrap_or_default(),
        _ => String::new(),
    };
    let mut textures = Vec::new();
    if let Some(buffer) = cluster.target(mat, "m_parameterBuffer") {
        for def in cluster.array_targets(buffer, "m_tweakableShaderParameterDefinitions") {
            if cluster.u32(def, "m_parameterType") != Some(TEXTURE_PARAMETER) {
                continue;
            }
            let Some(loc) = cluster.u32(def, "m_bufferLoc") else { continue };
            let base = loc & 0xF_FFFF;
            let parameter = cluster.string(def, "m_name").unwrap_or_default();
            // A texture value is a sampler slot followed by the texture pointer.
            for slot in [base + 12, base + 8] {
                let asset = match cluster.pointer_at(buffer, slot, 0) {
                    Some(Pointer::User(u)) => {
                        cluster.import(u).filter(|(_, c)| c.starts_with("PTexture")).map(|(id, _)| id)
                    }
                    Some(Pointer::Object { target, .. })
                        if cluster.class_name(target).starts_with("PTexture") =>
                    {
                        crate::texture::asset_name(cluster, target)
                    }
                    _ => None,
                };
                if let Some(asset) = asset {
                    textures.push(TextureBinding { parameter: parameter.clone(), asset });
                    break;
                }
            }
        }
    }
    Material { effect, textures }
}

fn segment(cluster: &Cluster, index: usize, seg: ObjectId) -> Result<Segment, MeshError> {
    let primitive = cluster.u32(seg, "m_primitiveType").unwrap_or(TRIANGLE_LIST);
    if primitive != TRIANGLE_LIST {
        return Err(MeshError::Primitive(index, primitive));
    }
    let vb = cluster.vertex_buffer.len();
    let ib = cluster.index_buffer.len();
    let mut streams = Vec::new();
    for block in cluster.array_targets(seg, "m_vertexData") {
        let stride = cluster.u32(block, "m_stride").unwrap_or(0);
        let count = cluster.u32(block, "m_elementCount").unwrap_or(0);
        let offset = cluster.u32(block, "m_offsetInVertexBuffer").unwrap_or(0) as usize;
        let size = cluster.u32(block, "m_dataSize").unwrap_or(stride * count) as usize;
        if offset + size > vb || (stride as usize * count as usize) > size {
            return Err(MeshError::OutOfRange(index));
        }
        for s in cluster.array_targets(block, "m_streams") {
            let semantic = match cluster.pointer(s, "m_renderDataType") {
                Some(Pointer::User(u)) => cluster.user_string(u),
                _ => String::new(),
            };
            let kind = cluster.u32(s, "m_type").unwrap_or(0);
            let format = VertexFormat::from_type(kind).ok_or_else(|| MeshError::UnknownStreamType {
                segment: index,
                semantic: semantic.clone(),
                kind,
            })?;
            let within = cluster.u32(s, "m_offset").unwrap_or(0) as usize;
            streams.push(VertexStream {
                semantic,
                format,
                stride,
                count,
                data: offset + within..offset + size,
            });
        }
    }
    let index_count = cluster.u32(seg, "m_indexData.m_elementCount").unwrap_or(0);
    let wide = cluster.u32(seg, "m_indexData.m_type") == Some(INDEX_U32);
    let start = cluster.u32(seg, "m_indexData.m_offsetInIndexBuffer").unwrap_or(0) as usize;
    let len = index_count as usize * if wide { 4 } else { 2 };
    if start + len > ib {
        return Err(MeshError::OutOfRange(index));
    }
    let skin_bones = cluster
        .array_targets(seg, "m_skinBones")
        .into_iter()
        .map(|b| {
            (
                cluster.u32(b, "m_hierarchyMatrixIndex").unwrap_or(0) as u16,
                cluster.u32(b, "m_skeletonMatrixIndex").unwrap_or(0) as u16,
            )
        })
        .collect();
    Ok(Segment {
        material: cluster.u32(seg, "m_materialIndex").unwrap_or(0),
        streams,
        index_count,
        wide_indices: wide,
        indices: start..start + len,
        max_index: cluster.u32(seg, "m_indexData.m_maximumIndex").unwrap_or(0),
        skin_bones,
    })
}

/// Read every mesh instance of a model cluster.
pub fn read_model(cluster: &Cluster) -> Result<Model, MeshError> {
    let mut meshes = Vec::new();
    for inst in cluster.objects("PMeshInstance") {
        let Some(mesh) = cluster.target(inst, "m_mesh") else { continue };
        let world = instance_world(cluster, inst);
        let mut segments = Vec::new();
        for (i, seg) in cluster.array_targets(mesh, "m_meshSegments").into_iter().enumerate() {
            segments.push(segment(cluster, i, seg)?);
        }
        // The instance's material set, else the mesh's defaults.
        let mut mats: Vec<ObjectId> = cluster.array_targets(mesh, "m_defaultMaterials");
        if mats.is_empty()
            && let Some(Pointer::Object { target, .. }) = cluster.pointer(inst, "m_materialSet")
            && cluster.class_name(target) == "PMaterial"
        {
            mats.push(target);
        }
        let materials = mats.into_iter().map(|m| material(cluster, m)).collect();
        let (bounds_min, bounds_size) = cluster
            .target(inst, "m_bounds")
            .and_then(|b| Some((cluster.floats(b, "m_min", 3)?, cluster.floats(b, "m_size", 3)?)))
            .map(|(a, b)| ([a[0], a[1], a[2]], [b[0], b[1], b[2]]))
            .unwrap_or(([0.0; 3], [0.0; 3]));
        meshes.push(Mesh {
            name: cluster.string(inst, "m_name").unwrap_or_default(),
            instance: inst,
            mesh,
            world,
            segments,
            materials,
            bounds_min,
            bounds_size,
        });
    }
    Ok(Model { meshes })
}

/// Read a stream as floats (`n` components per vertex; U8x4 values become 0–255 floats).
pub fn stream_floats(cluster: &Cluster, s: &VertexStream) -> Vec<f32> {
    let data = cluster.vertex_buffer().get(s.data.clone()).unwrap_or(&[]);
    let mut out = Vec::with_capacity(s.count as usize * 4);
    for v in 0..s.count as usize {
        let at = v * s.stride as usize;
        let Some(bytes) = data.get(at..at + s.format.size()) else { break };
        match s.format {
            VertexFormat::U8x4 => out.extend(bytes.iter().map(|&b| f32::from(b))),
            _ => out.extend(bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))),
        }
    }
    out
}

/// Indices of a segment widened to u32.
pub fn segment_indices(cluster: &Cluster, s: &Segment) -> Vec<u32> {
    let data = cluster.index_buffer().get(s.indices.clone()).unwrap_or(&[]);
    if s.wide_indices {
        data.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    } else {
        data.chunks_exact(2).map(|c| u32::from(u16::from_le_bytes([c[0], c[1]]))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_paths() {
        assert_eq!(
            texture_cluster_path(
                "ffx_data/gamedata/ps3data/chr/mon/m001/mdl/d3d11/m001.dae.phyre",
                "PS3Data/chr/mon/m001/tex/m001.dds"
            ),
            "ffx_data/gamedata/ps3data/chr/mon/m001/tex/d3d11/m001.dds.phyre"
        );
        assert_eq!(
            texture_cluster_path(
                "ffx-2_data/gamedata/ps3data/map/a/a00/mdl/d3d11/x.dae.phyre",
                "PS3Data/map/a/tex/B.dds"
            ),
            "ffx-2_data/gamedata/ps3data/map/a/tex/d3d11/b.dds.phyre"
        );
    }

    #[test]
    fn vertex_formats() {
        assert_eq!(VertexFormat::from_type(2), Some(VertexFormat::F32x3));
        assert_eq!(VertexFormat::from_type(19).map(VertexFormat::size), Some(4));
        assert_eq!(VertexFormat::from_type(7), None);
    }
}
