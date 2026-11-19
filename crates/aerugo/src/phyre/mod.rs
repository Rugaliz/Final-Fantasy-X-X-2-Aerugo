//! The remaster's renderer on its own shaders: effect passes translated to programs, their constant buffers
//! filled by variable name, Direct3D 11 state from the effect files, and frames written in the same form as
//! the oracle's captures of the original (`AERUGO_FRAME_DUMP=<file>`), so the two can be compared draw for draw.

// Parts of this module are used only by the game-shot check until the field draws with it.
#![allow(dead_code)]

pub mod field;
pub mod model;
pub mod scene;

use std::collections::HashMap;
use std::rc::Rc;

use aerugo_formats::dxbc::{self, ConstantBuffer, ResourceKind, Shader, VarType};
use aerugo_formats::effect::{D3dState, InputElement};
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::Renderer;
use aerugo_gfx::game::{
    BindingKind, BlendDesc, DepthDesc, GameDraw, GameFrame, GameOp, ProgramDesc, ProgramHandle, RasterDesc,
    RenderState, Stage, StencilOps, TargetBlend, VertexAttribute,
};
use aerugo_vfs::Vfs;
use glam::Mat4;

/// FNV-1a 64 of a program's bytecode (the oracle names the original's shaders the same way).
pub fn fnv(data: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A translated effect pass.
#[derive(Debug)]
pub struct Program {
    pub handle: ProgramHandle,
    pub vs: Shader,
    pub ps: Shader,
    pub vs_hash: u64,
    pub ps_hash: u64,
    pub state: D3dState,
    pub inputs: Vec<InputElement>,
}

/// Where a program comes from: effect cluster path, scene pass kind, shader index (number of lights for the lit
/// effects), shader pass index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProgramKey {
    pub cluster: String,
    pub pass: String,
    pub shader: usize,
    pub index: usize,
}

/// Programs translated so far (failures are remembered too).
#[derive(Debug, Default)]
pub struct Programs {
    map: HashMap<ProgramKey, Option<Rc<Program>>>,
    /// Per effect cluster: its scene passes and how many shader passes each shader has.
    shapes: HashMap<String, Vec<(String, Vec<usize>)>>,
    /// Problems met (logged once each).
    pub problems: Vec<String>,
}

fn bindings(shader: &Shader, stage: Stage) -> Vec<(Stage, u32, BindingKind)> {
    shader
        .resources
        .iter()
        .filter_map(|r| {
            let (base, kind) = match r.kind {
                ResourceKind::ConstantBuffer => (0, BindingKind::ConstantBuffer),
                ResourceKind::Sampler => (aerugo_gfx::game::BINDING_SAMPLER, BindingKind::Sampler),
                ResourceKind::Texture => (aerugo_gfx::game::BINDING_SRV, BindingKind::Texture),
                _ => return None,
            };
            Some((stage, base + r.bind_point, kind))
        })
        .collect()
}

impl Programs {
    /// Each translated program's bytecode hashes (vertex, pixel).
    pub fn hashes(&self) -> HashMap<ProgramHandle, (u64, u64)> {
        self.map.values().flatten().map(|p| (p.handle, (p.vs_hash, p.ps_hash))).collect()
    }

    /// The program of an effect pass, translated on first use.
    pub fn get(&mut self, vfs: &Vfs, renderer: &mut Renderer, key: &ProgramKey) -> Option<Rc<Program>> {
        if let Some(p) = self.map.get(key) {
            return p.clone();
        }
        let built = self.build(vfs, renderer, key);
        let p = match built {
            Ok(p) => Some(Rc::new(p)),
            Err(e) => {
                let msg = format!("{} [{} #{} pass {}]: {e}", key.cluster, key.pass, key.shader, key.index);
                log::warn!("{msg}");
                self.problems.push(msg);
                None
            }
        };
        self.map.insert(key.clone(), p.clone());
        p
    }

    fn build(&mut self, vfs: &Vfs, renderer: &mut Renderer, key: &ProgramKey) -> Result<Program, String> {
        let cluster =
            Cluster::parse(vfs.read(&key.cluster).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let effect = aerugo_formats::effect::read_effect(&cluster).ok_or("no effect variant")?;
        let sp = effect.scene_passes.iter().find(|s| s.kind == key.pass).ok_or("no such scene pass")?;
        let pass = sp.shaders.get(key.shader).and_then(|s| s.get(key.index)).ok_or("no such shader pass")?;
        let (Some(vs_code), Some(ps_code)) = (&pass.vertex, &pass.fragment) else {
            return Err("pass without vertex or pixel program".into());
        };
        let vs = dxbc::read_shader(vs_code).map_err(|e| e.to_string())?;
        let ps = dxbc::read_shader(ps_code).map_err(|e| e.to_string())?;
        let fc = renderer.float_controls();
        let float = aerugo_dxbc::FloatControls {
            independent_denorm: fc.independent_denorm,
            rte32: fc.rte32,
            rtz32: fc.rtz32,
            flush_denorm32: fc.flush_denorm32,
            preserve_denorm32: fc.preserve_denorm32,
            preserve_inf_nan32: fc.preserve_inf_nan32,
        };
        let vertex = aerugo_dxbc::to_spirv(vs_code, None, &float).map_err(|e| e.to_string())?;
        let pixel = aerugo_dxbc::to_spirv(ps_code, Some(vs_code), &float).map_err(|e| e.to_string())?;
        let mut all = bindings(&vs, Stage::Vertex);
        all.extend(bindings(&ps, Stage::Pixel));
        // Vertex inputs: each input register reads the layout element of the same semantic.
        let mut attributes = Vec::new();
        for e in vs.inputs.iter().filter(|e| e.system_value == 0) {
            let Some(l) = pass
                .inputs
                .iter()
                .find(|l| l.semantic.eq_ignore_ascii_case(&e.semantic) && l.index == e.index)
            else {
                return Err(format!("no input layout element for {}{}", e.semantic, e.index));
            };
            attributes.push(VertexAttribute {
                location: e.register,
                slot: l.slot,
                dxgi_format: l.dxgi_format,
                offset: 0,
            });
        }
        let handle = renderer
            .create_program(&ProgramDesc { vertex, pixel, bindings: all, attributes })
            .map_err(|e| format!("{e:#}"))?;
        Ok(Program {
            handle,
            vs_hash: fnv(vs_code),
            ps_hash: fnv(ps_code),
            vs,
            ps,
            state: pass.state,
            inputs: pass.inputs.clone(),
        })
    }
}

/// The renderer's state description of an effect pass's Direct3D 11 state.
pub fn render_state(s: &D3dState) -> RenderState {
    let r = s.raster;
    let d = s.depth;
    let ops = |i: usize| StencilOps { fail: d[i], depth_fail: d[i + 1], pass: d[i + 2], func: d[i + 3] };
    let mut targets = [TargetBlend::default(); 8];
    for (t, raw) in targets.iter_mut().zip(s.targets) {
        *t = TargetBlend {
            enable: raw[0] != 0,
            src: raw[1],
            dst: raw[2],
            op: raw[3],
            src_alpha: raw[4],
            dst_alpha: raw[5],
            op_alpha: raw[6],
            write_mask: raw[7] as u8,
        };
    }
    RenderState {
        raster: RasterDesc {
            fill: r[0],
            cull: r[1],
            front_ccw: r[2] != 0,
            depth_bias: r[3] as i32,
            depth_bias_clamp: r[4],
            slope_scaled_depth_bias: r[5],
            depth_clip: r[6] != 0,
            scissor: r[7] != 0,
            multisample: r[8] != 0,
        },
        depth: DepthDesc {
            enable: d[0] != 0,
            write: d[1] != 0,
            func: d[2],
            stencil: d[3] != 0,
            read_mask: d[4] as u8,
            write_mask: d[5] as u8,
            front: ops(6),
            back: ops(10),
        },
        blend: BlendDesc { alpha_to_coverage: s.blend[0] != 0, independent: s.blend[1] != 0, targets },
    }
}

/// A value for a shader variable.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Floats in declaration order (a vector, or several array elements back to back, one element per register).
    Floats(Vec<f32>),
    /// A matrix in column-vector convention (glam); written as the effects' HLSL declares it.
    Matrix(Mat4),
    /// Several matrices (an array).
    Matrices(Vec<Mat4>),
    /// Raw 32-bit words (ints, bools, switches).
    Words(Vec<u32>),
}

fn put(out: &mut [u8], at: usize, w: u32) {
    if let Some(b) = out.get_mut(at..at + 4) {
        b.copy_from_slice(&w.to_le_bytes());
    }
}

/// Write one matrix (column-vector convention `m`) at `at` as an HLSL `rows`×`cols` matrix of `class` (2 row-major,
/// 3 column-major). The effects multiply row vectors by their matrices (`mul(v, M)`), so the HLSL matrix is the
/// transpose of `m`: HLSL element [r][c] is `m`'s [c][r].
fn put_matrix(out: &mut [u8], at: usize, ty: &VarType, m: &Mat4) {
    let (rows, cols) = (usize::from(ty.rows), usize::from(ty.columns));
    let hlsl = |r: usize, c: usize| m.col(r)[c];
    for r in 0..rows {
        for c in 0..cols {
            let reg_at = if ty.class == 2 { at + r * 16 + c * 4 } else { at + c * 16 + r * 4 };
            put(out, reg_at, hlsl(r, c).to_bits());
        }
    }
}

fn register_size(ty: &VarType) -> usize {
    match ty.class {
        2 => usize::from(ty.rows) * 16,
        3 => usize::from(ty.columns) * 16,
        _ => 16,
    }
}

fn write_value(out: &mut [u8], at: usize, ty: &VarType, v: &Value) {
    let elements = usize::from(ty.elements.max(1));
    match v {
        Value::Matrix(m) => put_matrix(out, at, ty, m),
        Value::Matrices(ms) => {
            for (i, m) in ms.iter().take(elements).enumerate() {
                put_matrix(out, at + i * register_size(ty), ty, m);
            }
        }
        Value::Floats(f) => {
            let per = usize::from(ty.rows.max(1)) * usize::from(ty.columns.max(1));
            if elements > 1 || ty.class == 2 || ty.class == 3 {
                for (i, chunk) in f.chunks(per.max(1)).take(elements).enumerate() {
                    for (k, x) in chunk.iter().enumerate() {
                        put(out, at + i * 16 + k * 4, x.to_bits());
                    }
                }
            } else {
                for (k, x) in f.iter().take(per).enumerate() {
                    put(out, at + k * 4, x.to_bits());
                }
            }
        }
        Value::Words(w) => {
            for (k, x) in w.iter().enumerate() {
                put(out, at + k * 4, *x);
            }
        }
    }
}

/// Fill a constant buffer: every variable (struct members as `struct.member`) takes `value(name)`; names without a
/// value stay zero and are added to `missing`.
pub fn fill(
    cb: &ConstantBuffer,
    value: &mut dyn FnMut(&str) -> Option<Value>,
    missing: &mut Vec<String>,
) -> Vec<u8> {
    let mut out = vec![0u8; cb.size as usize];
    for v in &cb.variables {
        if v.ty.members.is_empty() {
            match value(&v.name) {
                Some(x) => write_value(&mut out, v.offset as usize, &v.ty, &x),
                None => missing.push(v.name.clone()),
            }
        } else {
            for m in &v.ty.members {
                let name = format!("{}.{}", v.name, m.name);
                match value(&name) {
                    Some(x) => write_value(&mut out, (v.offset + m.offset) as usize, &m.ty, &x),
                    None => missing.push(name),
                }
            }
        }
    }
    out
}

/// Write `frame` as the oracle writes its captures (`AEGPU002`), with every draw's program hashes from
/// `hashes` (program handle → (vertex, pixel)).
pub fn dump_frame(
    frame: &GameFrame,
    hashes: &HashMap<ProgramHandle, (u64, u64)>,
    path: &str,
) -> std::io::Result<()> {
    let mut b: Vec<u8> = b"AEGPU002".to_vec();
    let w32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
    for op in &frame.ops {
        let GameOp::Draw(d) = op else {
            let text = format!("{op:?}");
            let text: String = text.chars().take(200).collect();
            b.push(b'N');
            w32(&mut b, text.len() as u32);
            b.extend_from_slice(text.as_bytes());
            continue;
        };
        let d: &GameDraw = d;
        let (vs, ps) = hashes.get(&d.program).copied().unwrap_or_default();
        b.push(b'D');
        let kind = if d.index.is_some() { 0 } else { 1 };
        for v in [kind, d.count, d.first, d.base_vertex as u32, d.instances, d.topology] {
            w32(&mut b, v);
        }
        b.extend_from_slice(&vs.to_le_bytes());
        b.extend_from_slice(&ps.to_le_bytes());
        w32(&mut b, d.constants.len() as u32);
        for (stage, slot, data) in &d.constants {
            b.push(u8::from(*stage == Stage::Vertex));
            b.push(*slot as u8);
            w32(&mut b, data.len() as u32);
            b.extend_from_slice(data);
        }
        w32(&mut b, d.images.len() as u32);
        for (stage, slot, _) in &d.images {
            b.push(u8::from(*stage == Stage::Vertex));
            b.push(*slot as u8);
            b.extend_from_slice(&[0; 4 + 44 + 8]);
        }
        w32(&mut b, d.samplers.len() as u32);
        for (stage, slot, s) in &d.samplers {
            b.push(u8::from(*stage == Stage::Vertex));
            b.push(*slot as u8);
            for v in [
                s.filter,
                s.address[0],
                s.address[1],
                s.address[2],
                s.mip_lod_bias,
                s.max_anisotropy,
                s.comparison,
            ]
            .into_iter()
            .chain(s.border)
            .chain([s.min_lod, s.max_lod])
            {
                w32(&mut b, v);
            }
        }
        let st = &d.state;
        let mut blend = vec![0u8; 264];
        blend[0..4].copy_from_slice(&u32::from(st.blend.alpha_to_coverage).to_le_bytes());
        blend[4..8].copy_from_slice(&u32::from(st.blend.independent).to_le_bytes());
        for (i, t) in st.blend.targets.iter().enumerate() {
            let at = 8 + i * 32;
            for (k, v) in [u32::from(t.enable), t.src, t.dst, t.op, t.src_alpha, t.dst_alpha, t.op_alpha]
                .iter()
                .enumerate()
            {
                blend[at + k * 4..at + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            blend[at + 28] = t.write_mask;
        }
        b.extend_from_slice(&blend);
        for f in d.blend_factor {
            b.extend_from_slice(&f.to_le_bytes());
        }
        w32(&mut b, d.sample_mask);
        let dd = &st.depth;
        let mut depth = Vec::new();
        for v in [u32::from(dd.enable), u32::from(dd.write), dd.func, u32::from(dd.stencil)] {
            depth.extend_from_slice(&v.to_le_bytes());
        }
        depth.extend_from_slice(&[dd.read_mask, dd.write_mask, 0, 0]);
        for o in [dd.front, dd.back] {
            for v in [o.fail, o.depth_fail, o.pass, o.func] {
                depth.extend_from_slice(&v.to_le_bytes());
            }
        }
        b.extend_from_slice(&depth);
        w32(&mut b, d.stencil_ref);
        let r = &st.raster;
        for v in [
            r.fill,
            r.cull,
            u32::from(r.front_ccw),
            r.depth_bias as u32,
            r.depth_bias_clamp,
            r.slope_scaled_depth_bias,
            u32::from(r.depth_clip),
            u32::from(r.scissor),
            u32::from(r.multisample),
            0,
        ] {
            w32(&mut b, v);
        }
        for slot in 0..8u32 {
            let vb = d.vertex_buffers.iter().find(|v| v.slot == slot);
            for v in
                [u32::from(vb.is_some()), vb.map_or(0, |v| v.stride), vb.map_or(0, |v| v.offset as u32), 0]
            {
                w32(&mut b, v);
            }
        }
        for v in [u32::from(d.index.is_some()), if d.index.is_some_and(|i| i.wide) { 42 } else { 57 }, 0] {
            w32(&mut b, v);
        }
        w32(&mut b, 0);
        for _ in 0..9 {
            b.extend_from_slice(&[0; 48]);
        }
        for f in d.viewport {
            b.extend_from_slice(&f.to_le_bytes());
        }
    }
    std::fs::write(path, b)
}
