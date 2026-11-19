//! Reflection of compiled Direct3D 11 shaders (DXBC containers, Shader Model 4–5): the stage, the constant
//! buffers with their variables (names, offsets, types, struct members), the bound resources and the input and
//! output signatures. Layout as Microsoft's `d3d11shader.h` reflection structures describe it and as Wine's
//! d3dcompiler and vkd3d-shader read it. The code itself is translated by `aerugo-dxbc`.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DxbcError {
    #[error("not a DXBC container")]
    NotDxbc,
    #[error("truncated {0}")]
    Truncated(&'static str),
    #[error("no shader code chunk")]
    NoCode,
}

type Result<T> = std::result::Result<T, DxbcError>;

/// Shader stage, from the program version token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Domain,
    Compute,
}

/// Kind of a bound resource (`D3D_SHADER_INPUT_TYPE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    ConstantBuffer,
    TextureBuffer,
    Texture,
    Sampler,
    UavTyped,
    Structured,
    UavStructured,
    ByteAddress,
    UavByteAddress,
    UavAppendStructured,
    UavConsumeStructured,
    UavStructuredWithCounter,
    Other(u32),
}

impl ResourceKind {
    fn from_raw(v: u32) -> ResourceKind {
        use ResourceKind::*;
        match v {
            0 => ConstantBuffer,
            1 => TextureBuffer,
            2 => Texture,
            3 => Sampler,
            4 => UavTyped,
            5 => Structured,
            6 => UavStructured,
            7 => ByteAddress,
            8 => UavByteAddress,
            9 => UavAppendStructured,
            10 => UavConsumeStructured,
            11 => UavStructuredWithCounter,
            other => Other(other),
        }
    }
}

/// A resource the shader binds (`register(b|t|s|u<n>)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub name: String,
    pub kind: ResourceKind,
    /// `D3D_SRV_DIMENSION` (2 = 2D texture, 9 = cube, …).
    pub dimension: u32,
    pub bind_point: u32,
    pub bind_count: u32,
}

/// Type of a constant-buffer variable or struct member (`D3D11_SHADER_TYPE_DESC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarType {
    /// `D3D_SHADER_VARIABLE_CLASS`: 0 scalar, 1 vector, 2 row-major matrix, 3 column-major matrix, 5 struct.
    pub class: u16,
    /// `D3D_SHADER_VARIABLE_TYPE`: 1 bool, 2 int, 3 float, 19 uint, …
    pub base: u16,
    pub rows: u16,
    pub columns: u16,
    /// Array length; 0 when not an array.
    pub elements: u16,
    pub members: Vec<Member>,
}

/// A struct member: name, byte offset within the struct and type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub offset: u32,
    pub ty: VarType,
}

/// A constant-buffer variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    /// `D3D_SVF_USED`: the code reads it.
    pub used: bool,
    pub ty: VarType,
}

/// A constant buffer (`cbuffer`), e.g. `$Globals`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstantBuffer {
    pub name: String,
    pub size: u32,
    pub variables: Vec<Variable>,
}

/// One element of an input or output signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureElement {
    pub semantic: String,
    pub index: u32,
    /// `D3D_NAME` system value (0 none, 1 position, …, 64 target).
    pub system_value: u32,
    /// `D3D_REGISTER_COMPONENT_TYPE`: 1 uint, 2 int, 3 float.
    pub component_type: u32,
    pub register: u32,
    pub mask: u8,
    /// Components the shader reads (inputs) or does not always write (outputs).
    pub used_mask: u8,
    pub stream: u32,
}

/// What Aerugo reads from a shader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shader {
    pub stage: Stage,
    pub version: (u8, u8),
    pub constant_buffers: Vec<ConstantBuffer>,
    pub resources: Vec<Resource>,
    pub inputs: Vec<SignatureElement>,
    pub outputs: Vec<SignatureElement>,
}

impl Shader {
    pub fn constant_buffer(&self, name: &str) -> Option<&ConstantBuffer> {
        self.constant_buffers.iter().find(|c| c.name == name)
    }

    pub fn resource(&self, name: &str) -> Option<&Resource> {
        self.resources.iter().find(|r| r.name == name)
    }
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn cstr(d: &[u8], o: usize) -> Option<String> {
    let rest = d.get(o..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

/// The chunks of a container: (four-character code, data).
pub fn chunks(code: &[u8]) -> Result<Vec<([u8; 4], &[u8])>> {
    if code.len() < 32 || !code.starts_with(b"DXBC") {
        return Err(DxbcError::NotDxbc);
    }
    let total = u32_at(code, 24).ok_or(DxbcError::NotDxbc)? as usize;
    let code = code.get(..total).ok_or(DxbcError::Truncated("container"))?;
    let count = u32_at(code, 28).ok_or(DxbcError::Truncated("container"))? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = u32_at(code, 32 + 4 * i).ok_or(DxbcError::Truncated("chunk table"))? as usize;
        let fourcc: [u8; 4] = code.get(at..at + 4).ok_or(DxbcError::Truncated("chunk"))?.try_into().unwrap();
        let len = u32_at(code, at + 4).ok_or(DxbcError::Truncated("chunk"))? as usize;
        let data = code.get(at + 8..at + 8 + len).ok_or(DxbcError::Truncated("chunk"))?;
        out.push((fourcc, data));
    }
    Ok(out)
}

const MAX_TYPE_DEPTH: usize = 16;

fn read_type(d: &[u8], at: usize, depth: usize) -> Option<VarType> {
    let member_count = u16_at(d, at + 10)?;
    let member_at = u32_at(d, at + 12)? as usize;
    let mut members = Vec::new();
    if depth < MAX_TYPE_DEPTH {
        for k in 0..member_count as usize {
            let m = member_at + 12 * k;
            members.push(Member {
                name: cstr(d, u32_at(d, m)? as usize)?,
                ty: read_type(d, u32_at(d, m + 4)? as usize, depth + 1)?,
                offset: u32_at(d, m + 8)?,
            });
        }
    }
    Some(VarType {
        class: u16_at(d, at)?,
        base: u16_at(d, at + 2)?,
        rows: u16_at(d, at + 4)?,
        columns: u16_at(d, at + 6)?,
        elements: u16_at(d, at + 8)?,
        members,
    })
}

type Reflection = (Vec<ConstantBuffer>, Vec<Resource>);

fn read_rdef(d: &[u8]) -> Option<Reflection> {
    let (cb_count, cb_at) = (u32_at(d, 0)? as usize, u32_at(d, 4)? as usize);
    let (res_count, res_at) = (u32_at(d, 8)? as usize, u32_at(d, 12)? as usize);
    let (minor, major) = (*d.get(16)?, *d.get(17)?);
    // Shader Model 5.1 binding descriptions also carry a register space and an id.
    let res_size = if (major, minor) >= (5, 1) { 40 } else { 32 };
    let mut resources = Vec::with_capacity(res_count);
    for i in 0..res_count {
        let r = res_at + res_size * i;
        resources.push(Resource {
            name: cstr(d, u32_at(d, r)? as usize)?,
            kind: ResourceKind::from_raw(u32_at(d, r + 4)?),
            dimension: u32_at(d, r + 12)?,
            bind_point: u32_at(d, r + 20)?,
            bind_count: u32_at(d, r + 24)?,
        });
    }
    // Variable descriptions grew from 24 to 40 bytes with Shader Model 5 (texture and sampler ranges).
    let var_size = if major >= 5 { 40 } else { 24 };
    let mut buffers = Vec::with_capacity(cb_count);
    for i in 0..cb_count {
        let c = cb_at + 24 * i;
        let (var_count, var_at) = (u32_at(d, c + 4)? as usize, u32_at(d, c + 8)? as usize);
        let mut variables = Vec::with_capacity(var_count);
        for k in 0..var_count {
            let v = var_at + var_size * k;
            variables.push(Variable {
                name: cstr(d, u32_at(d, v)? as usize)?,
                offset: u32_at(d, v + 4)?,
                size: u32_at(d, v + 8)?,
                used: u32_at(d, v + 12)? & 2 != 0,
                ty: read_type(d, u32_at(d, v + 16)? as usize, 0)?,
            });
        }
        buffers.push(ConstantBuffer {
            name: cstr(d, u32_at(d, c)? as usize)?,
            size: u32_at(d, c + 12)?,
            variables,
        });
    }
    Some((buffers, resources))
}

/// `with_stream`: the element layout of `ISG1`/`OSG1`/`OSG5`, which starts with a stream index.
fn read_signature(d: &[u8], with_stream: bool, min_precision: bool) -> Option<Vec<SignatureElement>> {
    let count = u32_at(d, 0)? as usize;
    let size = 24 + if with_stream { 4 } else { 0 } + if min_precision { 4 } else { 0 };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let mut e = 8 + size * i;
        let stream = if with_stream {
            e += 4;
            u32_at(d, e - 4)?
        } else {
            0
        };
        out.push(SignatureElement {
            semantic: cstr(d, u32_at(d, e)? as usize)?,
            index: u32_at(d, e + 4)?,
            system_value: u32_at(d, e + 8)?,
            component_type: u32_at(d, e + 12)?,
            register: u32_at(d, e + 16)?,
            mask: *d.get(e + 20)?,
            used_mask: *d.get(e + 21)?,
            stream,
        });
    }
    Some(out)
}

/// Read a shader's stage and reflection.
pub fn read_shader(code: &[u8]) -> Result<Shader> {
    let chunks = chunks(code)?;
    let find = |names: &[&[u8; 4]]| chunks.iter().find(|(c, _)| names.contains(&c)).map(|(c, d)| (*c, *d));
    let (_, shader) = find(&[b"SHDR", b"SHEX"]).ok_or(DxbcError::NoCode)?;
    let token = u32_at(shader, 0).ok_or(DxbcError::Truncated("code"))?;
    let stage = match token >> 16 {
        0 => Stage::Pixel,
        1 => Stage::Vertex,
        2 => Stage::Geometry,
        3 => Stage::Hull,
        4 => Stage::Domain,
        _ => Stage::Compute,
    };
    let version = (((token >> 4) & 0xF) as u8, (token & 0xF) as u8);
    let (constant_buffers, resources) = match find(&[b"RDEF"]) {
        Some((_, d)) => read_rdef(d).ok_or(DxbcError::Truncated("RDEF"))?,
        None => (Vec::new(), Vec::new()),
    };
    let signature = |plain: &[u8; 4], v1: &[u8; 4], v5: Option<&[u8; 4]>| -> Result<Vec<SignatureElement>> {
        let mut names: Vec<&[u8; 4]> = vec![plain, v1];
        names.extend(v5);
        match find(&names) {
            Some((c, d)) => {
                let (stream, precision) = match &c {
                    c if c == plain => (false, false),
                    c if c == v1 => (true, true),
                    _ => (true, false),
                };
                read_signature(d, stream, precision).ok_or(DxbcError::Truncated("signature"))
            }
            None => Ok(Vec::new()),
        }
    };
    Ok(Shader {
        stage,
        version,
        constant_buffers,
        resources,
        inputs: signature(b"ISGN", b"ISG1", None)?,
        outputs: signature(b"OSGN", b"OSG1", Some(b"OSG5"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let header = 32 + 4 * chunks.len();
        let mut body = Vec::new();
        let mut offsets = Vec::new();
        for (cc, data) in chunks {
            offsets.push((header + body.len()) as u32);
            body.extend_from_slice(*cc);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
        }
        let mut out = b"DXBC".to_vec();
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&((header + body.len()) as u32).to_le_bytes());
        out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
        for o in offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn reads_stage_and_signature() {
        // vs_4_0 version token, then an input signature with one POSITION0 element at v0.
        let shdr = (0x0001_0040u32).to_le_bytes().to_vec();
        let mut isgn = Vec::new();
        for w in [1u32, 8, 32, 0, 0, 3, 0] {
            isgn.extend_from_slice(&w.to_le_bytes());
        }
        isgn.extend_from_slice(&[0x7, 0x7, 0, 0]);
        isgn.extend_from_slice(b"POSITION\0");
        let s = read_shader(&container(&[(b"ISGN", isgn), (b"SHDR", shdr)])).unwrap();
        assert_eq!(s.stage, Stage::Vertex);
        assert_eq!(s.version, (4, 0));
        assert_eq!(s.inputs.len(), 1);
        assert_eq!(
            (s.inputs[0].semantic.as_str(), s.inputs[0].register, s.inputs[0].mask),
            ("POSITION", 0, 7)
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(read_shader(b"nope"), Err(DxbcError::NotDxbc));
        let mut c = container(&[(b"SHDR", vec![0; 4])]);
        c.truncate(40);
        assert!(read_shader(&c).is_err());
    }

    /// Every program of every effect of both games reflects consistently: variables inside their buffers,
    /// every constant buffer bound, unique input registers.
    #[test]
    fn reflects_every_game_shader() {
        let Some(inst) = aerugo_vfs::install_from_env() else {
            eprintln!("skipped: set AERUGO_GAME_DIR");
            return;
        };
        let mut vfs = aerugo_vfs::Vfs::new();
        vfs.mount_install(&inst).unwrap();
        let paths: Vec<String> = vfs
            .archives()
            .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
            .filter(|p| p.contains("/shaders/d3d11/") && p.ends_with(".phyre"))
            .collect();
        let mut programs = 0;
        for path in &paths {
            let c = crate::phyre::Cluster::parse(vfs.read(path).unwrap()).unwrap();
            let Some(effect) = crate::effect::read_effect(&c) else { continue };
            for p in effect.scene_passes.iter().flat_map(|sp| &sp.programs) {
                for (code, stage) in [(&p.vertex, Stage::Vertex), (&p.fragment, Stage::Pixel)] {
                    let Some(code) = code else { continue };
                    programs += 1;
                    let s = read_shader(code).unwrap_or_else(|e| panic!("{path}: {e}"));
                    assert_eq!(s.stage, stage, "{path}");
                    for cb in &s.constant_buffers {
                        assert!(
                            s.resources
                                .iter()
                                .any(|r| r.kind == ResourceKind::ConstantBuffer && r.name == cb.name),
                            "{path}: {} unbound",
                            cb.name
                        );
                        for v in &cb.variables {
                            assert!(v.offset + v.size <= cb.size, "{path}: {} outside {}", v.name, cb.name);
                        }
                    }
                    let mut regs: Vec<u32> =
                        s.inputs.iter().filter(|e| e.system_value == 0).map(|e| e.register).collect();
                    regs.sort_unstable();
                    let n = regs.len();
                    regs.dedup();
                    if stage == Stage::Vertex {
                        assert_eq!(regs.len(), n, "{path}: shared vertex input registers");
                    }
                }
            }
        }
        eprintln!("{programs} programs reflected");
        assert!(programs > 4000);
    }
}
