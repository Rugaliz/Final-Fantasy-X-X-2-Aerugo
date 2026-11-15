//! Event and battle scripts: the bytecode the games' script machine runs. Event scripts (`*.ebp`) start with an
//! `EV01` header; battle scripts (`battle/btl/*/*.bin`, `battle/mon/*/*.bin`) are chunk containers whose first
//! chunk is the script. This module reads the script header and its workers and disassembles them by following
//! control flow from every entry point and label. See `docs/formats/script.md`.

mod arity;
mod natives;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::Game;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScriptError {
    #[error("not a script")]
    NotAScript,
    #[error("offset {0:#x} lies outside the file")]
    OutOfRange(usize),
}

type Result<T> = std::result::Result<T, ScriptError>;

fn u16_at(d: &[u8], at: usize) -> Result<u16> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(ScriptError::OutOfRange(at))
}

fn u32_at(d: &[u8], at: usize) -> Result<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(ScriptError::OutOfRange(at))
}

/// The element type of a script variable (bits 28–31 of its descriptor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    F32,
    Bits,
    Other(u8),
}

impl VarType {
    fn from_nibble(n: u8) -> VarType {
        match n {
            0 => VarType::U8,
            1 => VarType::I8,
            2 => VarType::U16,
            3 => VarType::I16,
            4 => VarType::U32,
            5 => VarType::I32,
            6 => VarType::F32,
            7 => VarType::Bits,
            n => VarType::Other(n),
        }
    }

    /// Bytes per element.
    pub fn size(self) -> u32 {
        match self {
            VarType::U8 | VarType::I8 | VarType::Bits => 1,
            VarType::U16 | VarType::I16 => 2,
            VarType::U32 | VarType::I32 | VarType::F32 => 4,
            VarType::Other(_) => 0,
        }
    }

    pub fn name(self) -> String {
        match self {
            VarType::U8 => "u8".into(),
            VarType::I8 => "i8".into(),
            VarType::U16 => "u16".into(),
            VarType::I16 => "i16".into(),
            VarType::U32 => "u32".into(),
            VarType::I32 => "i32".into(),
            VarType::F32 => "f32".into(),
            VarType::Bits => "bits".into(),
            VarType::Other(n) => format!("type{n}"),
        }
    }
}

/// A variable a worker can address: a storage area, an offset in it, an element type and an element count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Var {
    pub ty: VarType,
    /// Which memory the variable lives in (bits 25–27): 0/1 global blocks, 2–4 the worker's data areas, 5 the
    /// worker's registers, 6 the script's shared data.
    pub area: u8,
    pub offset: u32,
    pub count: u16,
}

/// One worker: a script entity with its own entry points, jump labels and variable view.
#[derive(Debug, Clone, PartialEq)]
pub struct Worker {
    pub kind: u8,
    pub vars: Vec<Var>,
    pub ints: Vec<i32>,
    pub floats: Vec<f32>,
    /// Entry offsets, relative to the start of the code.
    pub entries: Vec<u32>,
    /// Jump-label offsets, relative to the start of the code.
    pub labels: Vec<u32>,
    /// Offsets (from the script start) of the worker's three private data areas (variable areas 2, 3 and 4).
    pub data_areas: [u32; 3],
}

/// A parsed script.
#[derive(Debug, Clone)]
pub struct Script {
    data: Vec<u8>,
    code: usize,
    code_end: usize,
    /// Offset of the data shared by all workers (variable area 6).
    shared_data: u32,
    pub workers: Vec<Worker>,
}

/// What an instruction's 16-bit operand means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    None,
    Var,
    Label,
    Native,
    IntConst,
    Imm,
    FloatConst,
    Worker,
    Global,
}

/// Static facts about one opcode.
#[derive(Debug, Clone, Copy)]
pub struct OpInfo {
    pub mnemonic: &'static str,
    pub operand: Operand,
    /// Execution never falls through to the next instruction.
    pub ends_flow: bool,
    /// The operand is a label the instruction may jump to.
    pub branches: bool,
    pub valid: bool,
}

const fn op(mnemonic: &'static str, operand: Operand) -> OpInfo {
    OpInfo { mnemonic, operand, ends_flow: false, branches: false, valid: true }
}
const fn end(mnemonic: &'static str) -> OpInfo {
    OpInfo { mnemonic, operand: Operand::None, ends_flow: true, branches: false, valid: true }
}
const fn jump(mnemonic: &'static str, ends_flow: bool) -> OpInfo {
    OpInfo { mnemonic, operand: Operand::Label, ends_flow, branches: true, valid: true }
}
const BAD: OpInfo =
    OpInfo { mnemonic: "invalid", operand: Operand::None, ends_flow: true, branches: false, valid: false };

use Operand as O;

/// The instruction set: opcodes 0x00–0x7A (bit 7 of the byte only says a `u16` operand follows).
pub const OPS: [OpInfo; 0x7B] = [
    op("nop", O::None),
    op("or.logic", O::None),
    op("and.logic", O::None),
    op("or", O::None),
    op("xor", O::None),
    op("and", O::None),
    op("eq", O::None),
    op("ne", O::None),
    op("gt.u", O::None),
    op("lt.u", O::None),
    op("gt", O::None),
    op("lt", O::None),
    op("ge.u", O::None),
    op("le.u", O::None),
    op("ge", O::None),
    op("le", O::None),
    op("bit", O::None),
    op("bit.not", O::None),
    op("shl", O::None),
    op("shr", O::None),
    op("add", O::None),
    op("sub", O::None),
    op("mul", O::None),
    op("div", O::None),
    op("mod", O::None),
    op("not.logic", O::None),
    op("neg", O::None),
    BAD,
    op("not", O::None),
    op("nop", O::None),
    op("nop", O::None),
    op("push.var", O::Var),
    op("pop.var", O::Var),
    op("pop.var.b", O::Var),
    op("push.elem", O::Var),
    op("pop.elem", O::Var),
    op("pop.elem.b", O::Var),
    op("pop.fr", O::None),
    op("push.fr", O::None),
    op("push.addr", O::Var),
    op("push.ar", O::None),
    op("push.xr", O::None),
    op("pop.ar", O::None),
    op("dup", O::None),
    op("pop.xr", O::None),
    op("push.int", O::IntConst),
    op("push.imm", O::Imm),
    op("push.float", O::FloatConst),
    jump("jump", true),
    jump("jump.if", false),
    jump("jump.unless", false),
    op("call.worker", O::Worker),
    end("return"),
    op("call", O::Native),
    op("request", O::None),
    op("request.wait_start", O::None),
    op("request.wait_end", O::None),
    op("request.id", O::None),
    op("request.id.wait_start", O::None),
    op("request.id.wait_end", O::None),
    end("end"),
    end("end.cancel"),
    end("end.restart"),
    end("end.restart.cancel"),
    op("sync", O::None),
    BAD,
    BAD,
    BAD,
    BAD,
    op("request.45", O::None),
    op("request.46", O::None),
    op("request.47", O::None),
    op("request.48", O::None),
    op("request.49", O::None),
    op("request.wait_start.4a", O::None),
    op("request.wait_start.4b", O::None),
    op("request.wait_start.4c", O::None),
    op("request.wait_start.4d", O::None),
    op("request.wait_start.4e", O::None),
    op("request.wait_end.4f", O::None),
    op("request.wait_end.50", O::None),
    op("request.wait_end.51", O::None),
    op("request.wait_end.52", O::None),
    op("request.wait_end.53", O::None),
    end("end.cancel0"),
    jump("pop.ar.jump", true),
    jump("pop.ar.jump.if", false),
    jump("pop.ar.jump.unless", false),
    op("call.drop", O::Native),
    op("set.ri0", O::None),
    op("set.ri1", O::None),
    op("set.ri2", O::None),
    op("set.ri3", O::None),
    op("set.rf0", O::None),
    op("set.rf1", O::None),
    op("set.rf2", O::None),
    op("set.rf3", O::None),
    op("set.rf4", O::None),
    op("set.rf5", O::None),
    op("set.rf6", O::None),
    op("set.rf7", O::None),
    op("set.rf8", O::None),
    op("set.rf9", O::None),
    op("push.ri0", O::None),
    op("push.ri1", O::None),
    op("push.ri2", O::None),
    op("push.ri3", O::None),
    op("push.rf0", O::None),
    op("push.rf1", O::None),
    op("push.rf2", O::None),
    op("push.rf3", O::None),
    op("push.rf4", O::None),
    op("push.rf5", O::None),
    op("push.rf6", O::None),
    op("push.rf7", O::None),
    op("push.rf8", O::None),
    op("push.rf9", O::None),
    op("push.global", O::Global),
    op("nop", O::None),
    op("wait.request", O::None),
    op("wait.request.id", O::None),
    op("worker.op79", O::None),
    op("worker.op7a", O::None),
];

/// Facts about an opcode number (anything past the table is invalid).
pub fn op_info(op: u8) -> OpInfo {
    OPS.get(usize::from(op)).copied().unwrap_or(BAD)
}

/// One decoded instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction {
    /// Offset from the start of the code.
    pub pc: u32,
    pub op: u8,
    pub operand: Option<u16>,
}

impl Instruction {
    pub fn len(&self) -> u32 {
        if self.operand.is_some() { 3 } else { 1 }
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn info(&self) -> OpInfo {
        op_info(self.op)
    }
}

impl Script {
    /// Parse an event script (`EV01`) or a battle chunk container whose first chunk is a script.
    pub fn parse(data: &[u8]) -> Result<Script> {
        let base = if data.starts_with(b"EV01") {
            u32_at(data, 4)? as usize
        } else {
            let n = u32_at(data, 0).map_err(|_| ScriptError::NotAScript)?;
            if !(1..=32).contains(&n) {
                return Err(ScriptError::NotAScript);
            }
            u32_at(data, 4)? as usize
        };
        if base == 0 || base >= data.len() {
            return Err(ScriptError::NotAScript);
        }
        let d = &data[base..];
        let code = u32_at(d, 0x30)? as usize;
        // The section after the code starts at +0x2C; scripts without one leave it zero.
        let code_end = match u32_at(d, 0x2C)? as usize {
            0 => d.len(),
            end => end,
        };
        if code > code_end || code_end > d.len() {
            return Err(ScriptError::NotAScript);
        }
        let n = usize::from(u16_at(d, 0x34)?);
        let mut workers = Vec::with_capacity(n);
        for i in 0..n {
            workers.push(read_worker(d, u32_at(d, 0x38 + 4 * i)? as usize)?);
        }
        for w in &workers {
            if w.entries.iter().chain(&w.labels).any(|&o| code + o as usize > code_end) {
                return Err(ScriptError::NotAScript);
            }
        }
        let shared_data = u32_at(d, 0x20)?;
        Ok(Script { data: d.to_vec(), code, code_end, shared_data, workers })
    }

    /// The script's bytes, from its header (offsets in the script are relative to this).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Offset of the code from the script start.
    pub fn code_start(&self) -> u32 {
        self.code as u32
    }

    /// The map number the script names in its header (event scripts), if any.
    pub fn map_number(&self) -> Option<i16> {
        let table = u32_at(&self.data, 4).ok()? as usize;
        if table == 0 {
            return None;
        }
        u16_at(&self.data, table).ok().map(|v| v as i16)
    }

    /// Offset of the data shared by all workers.
    pub fn shared_data(&self) -> u32 {
        self.shared_data
    }

    /// Size of the code in bytes.
    pub fn code_len(&self) -> u32 {
        (self.code_end - self.code) as u32
    }

    /// Decode the instruction at `pc` (relative to the code start).
    pub fn instruction(&self, pc: u32) -> Option<Instruction> {
        let at = self.code + pc as usize;
        if at >= self.code_end {
            return None;
        }
        let byte = self.data[at];
        if byte & 0x80 != 0 {
            let operand = u16_at(&self.data[..self.code_end], at + 1).ok()?;
            Some(Instruction { pc, op: byte & 0x7F, operand: Some(operand) })
        } else {
            Some(Instruction { pc, op: byte, operand: None })
        }
    }

    /// Every instruction reachable from the worker's entries and labels, in address order.
    pub fn walk(&self, worker: usize) -> Vec<Instruction> {
        let Some(w) = self.workers.get(worker) else {
            return Vec::new();
        };
        let mut seen = BTreeMap::new();
        let mut todo: Vec<u32> = w.entries.iter().chain(&w.labels).copied().collect();
        while let Some(mut pc) = todo.pop() {
            while !seen.contains_key(&pc) {
                let Some(insn) = self.instruction(pc) else { break };
                seen.insert(pc, insn);
                let info = insn.info();
                if info.branches
                    && let Some(&target) = insn.operand.and_then(|l| w.labels.get(usize::from(l)))
                {
                    todo.push(target);
                }
                if info.ends_flow {
                    break;
                }
                pc += insn.len();
            }
        }
        seen.into_values().collect()
    }

    /// A text listing of one worker: header, variables, then the reachable code with entries and labels.
    pub fn disassemble(&self, worker: usize, game: Game) -> String {
        let mut out = String::new();
        let Some(w) = self.workers.get(worker) else {
            return out;
        };
        let _ = writeln!(
            out,
            "; worker {worker}  kind {}  {} entries  {} labels  {} variables",
            w.kind,
            w.entries.len(),
            w.labels.len(),
            w.vars.len()
        );
        for (i, v) in w.vars.iter().enumerate() {
            let count = if v.count > 1 { format!("[{}]", v.count) } else { String::new() };
            let _ = writeln!(out, ";   var{i}: {}{count} area {} +{:#x}", v.ty.name(), v.area, v.offset);
        }
        let mut tags: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        for (i, &e) in w.entries.iter().enumerate() {
            tags.entry(e).or_default().push(format!("entry{i}"));
        }
        for (i, &l) in w.labels.iter().enumerate() {
            tags.entry(l).or_default().push(format!("L{i}"));
        }
        let mut last_end = None;
        for insn in self.walk(worker) {
            if last_end.is_some_and(|e| e != insn.pc) {
                out.push('\n');
            }
            for t in tags.get(&insn.pc).into_iter().flatten() {
                let _ = writeln!(out, "{t}:");
            }
            let _ = writeln!(out, "  {:05x}  {}", insn.pc, self.format(&insn, w, game));
            last_end = Some(insn.pc + insn.len());
        }
        out
    }

    fn format(&self, insn: &Instruction, w: &Worker, game: Game) -> String {
        let info = insn.info();
        let Some(arg) = insn.operand else {
            return info.mnemonic.to_string();
        };
        let a = usize::from(arg);
        let text = match info.operand {
            O::Label => format!("L{arg}"),
            O::Var => format!("var{arg}"),
            O::Native => {
                let n = native(game, arg);
                format!("{}  ; {}", n.display(arg), n.signature())
            }
            O::IntConst => w.ints.get(a).map_or(format!("int#{arg}"), |v| v.to_string()),
            O::FloatConst => w.floats.get(a).map_or(format!("float#{arg}"), |v| format!("{v:?}")),
            O::Imm => (arg as i16).to_string(),
            O::Worker => format!("w{arg}"),
            O::Global => format!("g{arg}"),
            O::None => format!("{arg:#x}"),
        };
        format!("{:<12} {text}", info.mnemonic)
    }
}

fn read_worker(d: &[u8], at: usize) -> Result<Worker> {
    let kind = *d.get(at).ok_or(ScriptError::OutOfRange(at))?;
    let n_vars = usize::from(u16_at(d, at + 2)?);
    let n_ints = usize::from(u16_at(d, at + 4)?);
    let n_floats = usize::from(u16_at(d, at + 6)?);
    let n_entries = usize::from(u16_at(d, at + 8)?);
    let n_labels = usize::from(u16_at(d, at + 0xA)?);
    let table = |field: usize, n: usize| -> Result<Vec<u32>> {
        let start = u32_at(d, at + field)? as usize;
        (0..n).map(|i| u32_at(d, start + 4 * i)).collect()
    };
    let vars_at = u32_at(d, at + 0x14)? as usize;
    let vars = (0..n_vars)
        .map(|i| {
            let word = u32_at(d, vars_at + 8 * i)?;
            Ok(Var {
                ty: VarType::from_nibble((word >> 28) as u8),
                area: ((word >> 25) & 0x7) as u8,
                offset: word & 0x00FF_FFFF,
                count: u16_at(d, vars_at + 8 * i + 4)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let data_areas = [u32_at(d, at + 0x28)?, u32_at(d, at + 0x2C)?, u32_at(d, at + 0x30)?];
    Ok(Worker {
        data_areas,
        kind,
        vars,
        ints: table(0x18, n_ints)?.into_iter().map(|v| v as i32).collect(),
        floats: table(0x1C, n_floats)?.into_iter().map(f32::from_bits).collect(),
        entries: table(0x20, n_entries)?,
        labels: table(0x24, n_labels)?,
    })
}

/// What a native returns to the script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ret {
    None,
    Int,
    Float,
}

/// Our description of one native: its name and how many values it takes and returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeInfo {
    pub id: u16,
    pub name: &'static str,
    pub ints: u8,
    pub floats: u8,
    pub ret: Ret,
}

/// A native as shown to the user: named from our table, or by group and id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Native {
    pub info: Option<&'static NativeInfo>,
    pub group: &'static str,
}

impl Native {
    pub fn display(&self, id: u16) -> String {
        match self.info {
            Some(i) => i.name.to_string(),
            None => format!("{}.{id:04x}", self.group),
        }
    }

    pub fn signature(&self) -> String {
        match self.info {
            Some(i) => {
                let ret = match i.ret {
                    Ret::None => "",
                    Ret::Int => " -> int",
                    Ret::Float => " -> float",
                };
                format!("{}i {}f{ret}", i.ints, i.floats)
            }
            None => "unnamed".into(),
        }
    }
}

/// Native ids are `group << 12 | index`. Group names are ours.
pub fn native_group(game: Game, id: u16) -> &'static str {
    match (id >> 12, game) {
        (0, _) => "system",
        (1, _) => "math",
        (4, _) => "menu",
        (5, _) => "actor",
        (6, _) => "camera",
        (7, _) => "battle",
        (8, Game::Ffx) => "map",
        (8, Game::Ffx2) => "particles",
        (9, _) => "debug",
        (0xB, _) => "movie",
        (0xC, _) => "effect",
        (0xD, _) => "abilitymap",
        (0xE, _) => "dungeon",
        _ => "native",
    }
}

/// Look up a native in our table.
pub fn native(game: Game, id: u16) -> Native {
    let table = match game {
        Game::Ffx => natives::FFX,
        Game::Ffx2 => natives::FFX2,
    };
    let info = table.binary_search_by_key(&id, |n| n.id).ok().map(|i| &table[i]);
    Native { info, group: native_group(game, id) }
}

/// How many ints and floats a native takes and what it returns, for every native of the game.
pub fn arity(game: Game, id: u16) -> Option<(u8, u8, Ret)> {
    let corrections: &[arity::Arity] = match game {
        Game::Ffx => arity::FFX_CORRECTIONS,
        Game::Ffx2 => &[],
    };
    let table = match game {
        Game::Ffx => arity::FFX,
        Game::Ffx2 => arity::FFX2,
    };
    let found = corrections.iter().find(|a| a.0 == id).copied();
    found.or_else(|| table.binary_search_by_key(&id, |a| a.0).ok().map(|i| table[i])).map(|a| {
        let (_, ints, floats, ret) = a;
        (ints, floats, [Ret::None, Ret::Int, Ret::Float][usize::from(ret.min(2))])
    })
}

/// All named natives of a game.
pub fn named_natives(game: Game) -> &'static [NativeInfo] {
    match game {
        Game::Ffx => natives::FFX,
        Game::Ffx2 => natives::FFX2,
    }
}

/// Whether an archive path is a script: an event script, or a battle script (`btl/<x>/<x>.bin` or
/// `mon/_m<n>/m<n>.bin` under `battle/`). Files of the same name in the per-language `new_*` folders hold the
/// battle's text, not a script.
pub fn is_script_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    if p.ends_with(".ebp") {
        return true;
    }
    if p.contains("/new_") {
        return false;
    }
    let parts: Vec<&str> = p.rsplit('/').take(4).collect();
    if let [file, dir, kind, "battle"] = parts[..] {
        let stem = file.strip_suffix(".bin").unwrap_or("");
        return match kind {
            "btl" => stem == dir,
            "mon" => dir.strip_prefix('_') == Some(stem),
            _ => false,
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny script: one worker, entry 0 jumps over a native call to label 0, which ends.
    fn sample() -> Vec<u8> {
        let mut s = vec![0u8; 0x100];
        let put = |s: &mut Vec<u8>, at: usize, v: u32| s[at..at + 4].copy_from_slice(&v.to_le_bytes());
        // EV01 header: script at 0x40.
        s[..4].copy_from_slice(b"EV01");
        put(&mut s, 4, 0x40);
        let b = 0x40;
        let code = 0xA0u32;
        let bytes = [0x2E | 0x80, 5, 0, 0x30 | 0x80, 0, 0, 0xB5, 0x01, 0x10, 0x3C];
        put(&mut s, b + 0x30, code);
        put(&mut s, b + 0x2C, code + bytes.len() as u32);
        s[b + 0x34] = 1;
        put(&mut s, b + 0x38, 0x40);
        // Worker descriptor at script + 0x40: 1 var, 0 ints, 0 floats, 1 entry, 1 label.
        let w = b + 0x40;
        s[w] = 2;
        s[w + 2] = 1;
        s[w + 8] = 1;
        s[w + 0xA] = 1;
        put(&mut s, w + 0x14, 0x78);
        put(&mut s, w + 0x18, 0x88);
        put(&mut s, w + 0x1C, 0x88);
        put(&mut s, w + 0x20, 0x88);
        put(&mut s, w + 0x24, 0x8C);
        put(&mut s, b + 0x78, 0x6100_0010);
        s[b + 0x7C] = 3;
        put(&mut s, b + 0x88, 0);
        put(&mut s, b + 0x8C, 9);
        s[b + code as usize..b + code as usize + bytes.len()].copy_from_slice(&bytes);
        s
    }

    #[test]
    fn parses_and_follows_flow() {
        let s = Script::parse(&sample()).unwrap();
        assert_eq!(s.workers.len(), 1);
        let w = &s.workers[0];
        assert_eq!(w.vars[0], Var { ty: VarType::F32, area: 0, offset: 0x10, count: 3 });
        let pcs: Vec<u32> = s.walk(0).iter().map(|i| i.pc).collect();
        // The call at 6 is skipped by the jump; label 0 at 9 is reached.
        assert_eq!(pcs, vec![0, 3, 9]);
        let text = s.disassemble(0, Game::Ffx);
        assert!(text.contains("push.imm"), "{text}");
        assert!(text.contains("jump         L0"), "{text}");
        assert!(text.contains("L0:\n  00009  end"), "{text}");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(Script::parse(b"").unwrap_err(), ScriptError::NotAScript);
        assert!(Script::parse(&[0xFF; 64]).is_err());
        let mut s = sample();
        s[0x40 + 0x2C] = 0xFF; // code end past the file
        assert!(Script::parse(&s).is_err());
    }

    #[test]
    fn never_panics_on_corruption() {
        let base = sample();
        for i in 0..base.len() {
            for v in [0x00, 0x7F, 0x80, 0xFF] {
                let mut s = base.clone();
                s[i] = v;
                if let Ok(sc) = Script::parse(&s) {
                    for w in 0..sc.workers.len() {
                        let _ = sc.disassemble(w, Game::Ffx2);
                    }
                }
            }
        }
    }

    #[test]
    fn native_table_is_sorted_and_unique() {
        for game in Game::ALL {
            let t = named_natives(game);
            assert!(t.windows(2).all(|p| p[0].id < p[1].id));
            let mut names: Vec<_> = t.iter().map(|n| n.name).collect();
            names.sort_unstable();
            let before = names.len();
            names.dedup();
            assert_eq!(before, names.len(), "duplicate names in {}", game.name());
            assert!(t.iter().all(|n| n.name.contains('.') && n.name.is_ascii()));
        }
        assert_eq!(native(Game::Ffx, 0x0000).display(0), "system.wait");
        assert_eq!(native(Game::Ffx, 0x7FFF).display(0x7FFF), "battle.7fff");
    }

    #[test]
    fn script_paths() {
        assert!(is_script_path("ffx_ps2/ffx/master/jppc/event/obj/bj/bjyt0000/bjyt0000.ebp"));
        assert!(is_script_path("ffx_ps2/ffx/master/jppc/battle/btl/azit03_00/azit03_00.bin"));
        assert!(is_script_path("ffx_ps2/ffx/master/jppc/battle/mon/_m001/m001.bin"));
        assert!(!is_script_path("ffx_ps2/ffx/master/jppc/battle/kernel/arms_rate.bin"));
        assert!(!is_script_path("ffx_ps2/ffx/master/new_uspc/battle/btl/azit03_00/azit03_00.bin"));
        assert!(!is_script_path("ffx_ps2/ffx2/master/jppc/battle/btl/x/cam_x.src"));
    }
}
