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
    /// Which set of its semantic the stream belongs to (texture coordinate set 0, 1, … with its tangents).
    pub set: u8,
}

/// A shader input fed from a stream set other than the default, as a mesh instance binds it (the remaster's
/// map shaders name their inputs `Uv`, `Uv1`, `Uv_nmlmap1`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInput {
    /// Semantic of the stream (`ST`).
    pub semantic: String,
    /// Shader input name.
    pub name: String,
    /// Semantic index of the shader input (`TEXCOORD<index>`).
    pub index: u8,
    /// Stream set that feeds it.
    pub set: u8,
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
    /// The PS2 model part this segment belongs to (`m_modeIndex`): a character's part matrices go to the
    /// instances whose segments name the part.
    pub part: i32,
    /// The instance's input bindings for this segment (inputs not listed read set 0).
    pub inputs: Vec<StreamInput>,
}

impl Segment {
    pub fn stream(&self, semantic: &str) -> Option<&VertexStream> {
        self.streams.iter().find(|s| s.semantic == semantic)
    }

    /// The `n`-th stream of a semantic (a second UV set, its tangents).
    pub fn stream_nth(&self, semantic: &str, n: usize) -> Option<&VertexStream> {
        self.streams.iter().filter(|s| s.semantic == semantic).nth(n)
    }

    /// The stream set a shader input (semantic and index, e.g. `ST` 4 for `TEXCOORD4`) reads.
    pub fn input_set(&self, semantic: &str, index: u8) -> u8 {
        self.inputs.iter().find(|i| i.semantic == semantic && i.index == index).map_or(0, |i| i.set)
    }

    /// The stream of a semantic in a set; a segment with a single stream of that semantic feeds every set
    /// from it.
    pub fn stream_in_set(&self, semantic: &str, set: u8) -> Option<&VertexStream> {
        self.streams.iter().find(|s| s.semantic == semantic && s.set == set).or_else(|| {
            let mut all = self.streams.iter().filter(|s| s.semantic == semantic);
            let first = all.next();
            if all.next().is_none() { first } else { None }
        })
    }

    /// Position stream (`Vertex` or `SkinnableVertex`).
    pub fn positions(&self) -> Option<&VertexStream> {
        self.stream("Vertex").or_else(|| self.stream("SkinnableVertex"))
    }

    pub fn normals(&self) -> Option<&VertexStream> {
        self.stream("Normal").or_else(|| self.stream("SkinnableNormal"))
    }
}

/// A Phyre sampler state (`PSamplerState`): GL-style filters (0 nearest, 1 linear, 2–5 the mipmapped
/// combinations), wrap modes, LOD bias, anisotropy, border colour (RGBA bytes) and flags (bit 6: comparison).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SamplerState {
    pub min_filter: u8,
    pub mag_filter: u8,
    pub wrap: [u8; 3],
    pub lod_bias: f32,
    pub max_anisotropy: f32,
    pub border: [u8; 4],
    pub flags: u32,
}

/// Read a `PSamplerState` object (or one derived from it).
pub fn sampler_state(cluster: &Cluster, id: ObjectId) -> SamplerState {
    let b = |n: &str| cluster.u32(id, n).unwrap_or(0) as u8;
    let border = cluster.u32(id, "m_borderColor").unwrap_or(0).to_le_bytes();
    SamplerState {
        min_filter: b("m_minFilter"),
        mag_filter: b("m_magFilter"),
        wrap: [b("m_wrapS"), b("m_wrapT"), b("m_wrapR")],
        lod_bias: cluster.f32(id, "m_lodBias").unwrap_or(0.0),
        max_anisotropy: cluster.f32(id, "m_maxAnisotropy").unwrap_or(1.0),
        border,
        flags: cluster.u32(id, "m_flags").unwrap_or(0),
    }
}

/// A texture bound to a material parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureBinding {
    /// Shader parameter name (`TextureSampler`, `NormalMapSampler`, …).
    pub parameter: String,
    /// Asset id of the texture (`PS3Data/…/name.dds`), see [`texture_cluster_path`].
    pub asset: String,
    /// The sampler state the material gives it.
    pub sampler: Option<SamplerState>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Material {
    /// Effect asset id (`PS3Data/Shaders/<name>.fx#<variant>`).
    pub effect: String,
    pub textures: Vec<TextureBinding>,
    /// The material's numeric shader parameters by name (`MaterialColour`, `AlphaThreshold`, ...), as many floats
    /// as each takes.
    pub values: Vec<(String, Vec<f32>)>,
    /// The material's sampler-state parameters by name (`TextureSamplerState`, `globalSamplerState`, …).
    pub samplers: Vec<(String, SamplerState)>,
    /// The texture animation that drives its `TextureSampler` (`m_texAnimID`; empty for none).
    pub tex_anim_id: String,
}

impl Material {
    /// A numeric shader parameter.
    pub fn value(&self, name: &str) -> Option<&[f32]> {
        self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_slice())
    }

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
    /// The PS2 draw object kind (`m_DObjKind`): 1 marks a map's sky objects.
    pub kind: i32,
    /// The PS2 display object it belongs to (`m_objID`): keyframes on that object move and turn it.
    pub object: i32,
    /// The map group it belongs to (`m_groupID`): a map draws only the groups switched on.
    pub group: i32,
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
/// Parameter types of texture samplers: 2D, and cube maps.
const TEXTURE_PARAMETERS: [u32; 2] = [66, 68];
/// Parameter type of sampler states.
const SAMPLER_PARAMETER: u32 = 71;

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

/// The colour-correction table of the map a map model belongs to (`…/map/<area>/<map>/mdl/d3d11/<name>.phyre` →
/// `…/map/<area>/<map>/tex/d3d11/<map>_colorcorrection.dds.phyre`); many maps have none.
pub fn color_correction_path(model_path: &str) -> Option<String> {
    let lower = model_path.to_ascii_lowercase();
    let (dir, _) = lower.split_once("/mdl/")?;
    let map = dir.rsplit('/').next()?;
    Some(format!("{dir}/tex/d3d11/{map}_colorcorrection.dds.phyre"))
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
    let mut values = Vec::new();
    let mut samplers = Vec::new();
    if let Some(buffer) = cluster.target(mat, "m_parameterBuffer") {
        let bytes = cluster.object_bytes(buffer);
        for def in cluster.array_targets(buffer, "m_tweakableShaderParameterDefinitions") {
            let kind = cluster.u32(def, "m_parameterType");
            if kind == Some(SAMPLER_PARAMETER) {
                // A sampler state: a pointer 8 bytes into its value.
                let (Some(name), Some(loc)) =
                    (cluster.string(def, "m_name"), cluster.u32(def, "m_bufferLoc"))
                else {
                    continue;
                };
                if let Some(Pointer::Object { target, .. }) =
                    cluster.pointer_at(buffer, (loc & 0xF_FFFF) + 8, 0)
                    && cluster.class_name(target).starts_with("PSamplerState")
                {
                    samplers.push((name, sampler_state(cluster, target)));
                }
                continue;
            }
            if !kind.is_some_and(|t| TEXTURE_PARAMETERS.contains(&t)) {
                // Numbers sit in the buffer object at their buffer offset (the object's header fills offsets no
                // parameter uses); values past the object's end were left out of the file.
                let u16_at = |f: &[u8]| u16::from_le_bytes([f[0], f[1]]) as usize;
                let (Some(name), Some(off), Some(size)) = (
                    cluster.string(def, "m_name"),
                    cluster.field(def, "m_bufferLoc.m_offset").filter(|f| f.len() >= 2).map(u16_at),
                    cluster.field(def, "m_bufferLoc.m_size").filter(|f| f.len() >= 2).map(u16_at),
                ) else {
                    continue;
                };
                if let Some(raw) = bytes.get(off..off + size).filter(|_| size > 0 && size <= 64) {
                    let floats =
                        raw.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                    values.push((name, floats));
                }
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
                    // The value's sampler slot (8 bytes in) points at its sampler state.
                    let sampler = match cluster.pointer_at(buffer, base + 8, 0) {
                        Some(Pointer::Object { target, .. })
                            if cluster.class_name(target).starts_with("PSamplerState") =>
                        {
                            Some(sampler_state(cluster, target))
                        }
                        _ => None,
                    };
                    textures.push(TextureBinding { parameter: parameter.clone(), asset, sampler });
                    break;
                }
            }
        }
    }
    let tex_anim_id = cluster.string(mat, "m_texAnimID").unwrap_or_default();
    Material { effect, textures, values, samplers, tex_anim_id }
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
                set: cluster.u32(s, "m_streamSet").unwrap_or(0) as u8,
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
        part: cluster.u32(seg, "m_modeIndex").unwrap_or(0) as i32,
        inputs: Vec::new(),
    })
}

/// The stream bindings of one segment context of a mesh instance.
fn stream_inputs(cluster: &Cluster, context: ObjectId) -> Vec<StreamInput> {
    cluster
        .array_targets(context, "m_streamBindings")
        .into_iter()
        .map(|b| StreamInput {
            semantic: match cluster.pointer(b, "m_renderDataType") {
                Some(Pointer::User(u)) => cluster.user_string(u),
                _ => String::new(),
            },
            name: cluster.string(b, "m_name").unwrap_or_default(),
            index: cluster.u32(b, "m_index").unwrap_or(0) as u8,
            set: cluster.u32(b, "m_inputSet").unwrap_or(0) as u8,
        })
        .collect()
}

/// A map's scene settings: the remaster keeps them on the map cluster's camera named `GlobalCamera` (fog, the
/// background colour, glow, depth of field and the character light all come from here).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SceneSettings {
    pub near: f32,
    pub far: f32,
    pub ambient: [f32; 3],
    pub background: [f32; 3],
    pub fog_color: [f32; 3],
    pub fog_near: f32,
    pub fog_far: f32,
    pub fog_curve: f32,
    /// Fog limit as stored (0–255 scale).
    pub fog_limit: f32,
    /// Glow upper and lower threshold, gain, spread.
    pub glow: [f32; 4],
    /// Depth of field: focus plane distance, focus range, blur range.
    pub dof: [f32; 3],
    pub character_light_intensity: [f32; 4],
    pub character_light_pitch: [f32; 4],
    pub character_light_yaw: [f32; 4],
    /// Vertical field of view (radians).
    pub fov: f32,
}

/// The scene settings of a map cluster (its `GlobalCamera`), if it has them.
pub fn scene_settings(cluster: &Cluster) -> Option<SceneSettings> {
    let cam = cluster
        .objects("PCameraPerspective")
        .into_iter()
        .find(|&c| cluster.string(c, "m_name").as_deref() == Some("GlobalCamera"))?;
    let f = |name: &str| cluster.floats(cam, name, 1).and_then(|v| v.first().copied()).unwrap_or(0.0);
    let v4 = |name: &str| {
        cluster.floats(cam, &format!("{name}.m_elements"), 4).map_or([0.0; 4], |v| [v[0], v[1], v[2], v[3]])
    };
    let v3 = |name: &str| {
        let v = v4(name);
        [v[0], v[1], v[2]]
    };
    Some(SceneSettings {
        near: f("m_nearPlane"),
        far: f("m_farPlane"),
        ambient: v3("m_ambientColor"),
        background: v3("m_bgColor"),
        fog_color: v3("m_fogColor"),
        fog_near: f("m_fogNear"),
        fog_far: f("m_fogFar"),
        fog_curve: f("m_fogCurve"),
        fog_limit: f("m_fogLimit"),
        glow: [f("m_glowUpperThreshold"), f("m_glowLowerThreshold"), f("m_glowGain"), f("m_glowSpread")],
        dof: [f("m_dofFocusPlaneDistance"), f("m_dofFocusRange"), f("m_dofFocusBlurRange")],
        character_light_intensity: v4("m_charcterLightIntensity"),
        character_light_pitch: v4("m_charcterLightPitch"),
        character_light_yaw: v4("m_characterLightYaw"),
        fov: f("m_FOV"),
    })
}

/// Read every mesh instance of a model cluster.
pub fn read_model(cluster: &Cluster) -> Result<Model, MeshError> {
    let mut meshes = Vec::new();
    for inst in cluster.objects("PMeshInstance") {
        let Some(mesh) = cluster.target(inst, "m_mesh") else { continue };
        let world = instance_world(cluster, inst);
        let mut segments = Vec::new();
        let contexts = cluster.array_targets(inst, "m_segmentContext");
        for (i, seg) in cluster.array_targets(mesh, "m_meshSegments").into_iter().enumerate() {
            let mut s = segment(cluster, i, seg)?;
            if let Some(&c) = contexts.get(i) {
                s.inputs = stream_inputs(cluster, c);
            }
            segments.push(s);
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
        let kind = cluster.u32(inst, "m_DObjKind").unwrap_or(0) as i32;
        let object = cluster.u32(inst, "m_objID").unwrap_or(0) as i32;
        let group = cluster.u32(inst, "m_groupID").unwrap_or(0) as i32;
        meshes.push(Mesh {
            name: cluster.string(inst, "m_name").unwrap_or_default(),
            instance: inst,
            mesh,
            world,
            segments,
            materials,
            bounds_min,
            bounds_size,
            kind,
            object,
            group,
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
    fn color_correction_paths() {
        assert_eq!(
            color_correction_path("ffx_data/gamedata/ps3data/map/cdsp/cdsp07/mdl/d3d11/cdsp07.dae.phyre")
                .as_deref(),
            Some("ffx_data/gamedata/ps3data/map/cdsp/cdsp07/tex/d3d11/cdsp07_colorcorrection.dds.phyre")
        );
        assert_eq!(color_correction_path("ffx_data/gamedata/ps3data/chr/pc/c000.dae.phyre"), None);
    }

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
