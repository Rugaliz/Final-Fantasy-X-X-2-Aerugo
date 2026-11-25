//! The memory scripts address by variable descriptors: two global blocks (the first is the save image), the
//! script's own data (worker areas 2–4 and the shared area 6) and each worker's registers (area 5).
//!
//! A machine running several scripts as one group (a battle's) keeps each script's data, and its workers keep
//! their own work area for area 3 (AtelSetMyWorkAdrs with a buffer: the control's area-3 callback reads
//! worker +0xb8 instead of the script).

use aerugo_formats::script::{Var, VarType};

/// Size of each global block.
pub const GLOBAL_SIZE: usize = 0x1_0000;
/// Global block 0 is the save image; variables in area 0 start this far into it.
pub const AREA0_OFFSET: usize = 0x1EC;

/// Where a variable's bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Global(usize, usize),
    /// The data of the group's first script (the only one outside battles).
    Script(usize),
    /// The data of another script of the group (its index, 1 on).
    GroupScript(usize, usize),
    /// A worker's own work area (battle groups' area 3).
    Work(usize, usize),
    Registers(usize, usize),
}

impl Place {
    /// The place `n` bytes further on.
    pub fn offset(self, n: usize) -> Place {
        match self {
            Place::Global(b, o) => Place::Global(b, o + n),
            Place::Script(o) => Place::Script(o + n),
            Place::GroupScript(i, o) => Place::GroupScript(i, o + n),
            Place::Work(w, o) => Place::Work(w, o + n),
            Place::Registers(w, o) => Place::Registers(w, o + n),
        }
    }
}

/// Addresses handed to natives (`push.addr`): `area << 24 | offset`, with the worker in bits 12–23 for area 5,
/// a group's script in bits 20–23 for script data (0 the first script) and the worker in bits 16–23 for work
/// areas (area 3).
pub fn encode_address(place: Place) -> i32 {
    match place {
        Place::Global(b, off) => ((b as i32) << 24) | (off as i32 & 0xFF_FFFF),
        Place::Script(off) => (6 << 24) | (off as i32 & 0xF_FFFF),
        Place::GroupScript(i, off) => (6 << 24) | ((i as i32 & 0xF) << 20) | (off as i32 & 0xF_FFFF),
        Place::Work(w, off) => (3 << 24) | ((w as i32 & 0xFF) << 16) | (off as i32 & 0xFFFF),
        Place::Registers(w, off) => (5 << 24) | ((w as i32 & 0xFFF) << 12) | (off as i32 & 0xFFF),
    }
}

pub fn decode_address(a: i32) -> Place {
    let off = (a & 0xFF_FFFF) as usize;
    match (a >> 24) & 0x7 {
        0 => Place::Global(0, off),
        1 => Place::Global(1, off),
        3 => Place::Work(off >> 16, off & 0xFFFF),
        5 => Place::Registers((off >> 12) & 0xFFF, off & 0xFFF),
        _ => match off >> 20 {
            0 => Place::Script(off),
            i => Place::GroupScript(i, off & 0xF_FFFF),
        },
    }
}

/// All script-visible memory of a running event.
#[derive(Debug, Clone)]
pub struct Memory {
    pub global: [Vec<u8>; 2],
    /// A writable copy of the script (data areas live inside it).
    pub script: Vec<u8>,
    /// Writable copies of a group's other scripts (script 1 on).
    pub group_scripts: Vec<Vec<u8>>,
    /// Each worker's own work area, when it has one (battle groups).
    pub works: Vec<Option<Vec<u8>>>,
    /// Each worker's 14 registers as bytes (4 int, 10 float).
    pub registers: Vec<[u8; 56]>,
}

impl Memory {
    pub fn new(script: Vec<u8>, workers: usize) -> Memory {
        Memory {
            global: [vec![0; GLOBAL_SIZE], vec![0; GLOBAL_SIZE]],
            script,
            group_scripts: Vec::new(),
            works: vec![None; workers],
            registers: vec![[0; 56]; workers],
        }
    }

    /// The memory of a group: each script's data (the first as [`Memory::script`]) and, when `works` is given,
    /// each worker's zeroed work area of that size.
    pub fn group(mut scripts: Vec<Vec<u8>>, workers: usize, works: Option<Vec<usize>>) -> Memory {
        let first = if scripts.is_empty() { Vec::new() } else { scripts.remove(0) };
        let mut m = Memory::new(first, workers);
        m.group_scripts = scripts;
        if let Some(sizes) = works {
            m.works = sizes.into_iter().map(|n| Some(vec![0; n])).collect();
            m.works.resize(workers, None);
        }
        m
    }

    /// The bytes of element `index` of `var` as seen by `worker` of the group's script `script` (with its data
    /// areas).
    pub fn place(
        &self,
        var: &Var,
        index: i32,
        worker: usize,
        script: usize,
        areas: [u32; 3],
        shared: u32,
    ) -> Place {
        let size = var.ty.size().max(1) as usize;
        let index = index.clamp(0, i32::from(var.count.max(1)) - 1) as usize;
        let off = var.offset as usize + index * size;
        let in_script = |o: usize| if script == 0 { Place::Script(o) } else { Place::GroupScript(script, o) };
        match var.area {
            0 => Place::Global(0, AREA0_OFFSET + off),
            1 => Place::Global(1, off),
            3 if self.works.get(worker).is_some_and(Option::is_some) => Place::Work(worker, off),
            2..=4 => in_script(areas[usize::from(var.area - 2)] as usize + off),
            5 => Place::Registers(worker, off),
            _ => in_script(shared as usize + off),
        }
    }

    /// `n` bytes at a place (none when they run past its block).
    pub fn bytes_mut(&mut self, p: Place, n: usize) -> Option<&mut [u8]> {
        self.bytes(p, n)
    }

    /// The bytes at a place up to (not including) the first NUL, at most `max`.
    pub fn c_string(&mut self, p: Place, max: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for k in 0..max {
            let q = p.offset(k);
            match self.bytes(q, 1).map(|b| b[0]) {
                Some(0) | None => break,
                Some(b) => out.push(b),
            }
        }
        out
    }

    fn bytes(&mut self, p: Place, n: usize) -> Option<&mut [u8]> {
        match p {
            Place::Global(b, off) => self.global[b].get_mut(off..off + n),
            Place::Script(off) => self.script.get_mut(off..off + n),
            Place::GroupScript(i, off) => {
                self.group_scripts.get_mut(i.checked_sub(1)?)?.get_mut(off..off + n)
            }
            Place::Work(w, off) => self.works.get_mut(w)?.as_mut()?.get_mut(off..off + n),
            Place::Registers(w, off) => self.registers.get_mut(w)?.get_mut(off..off + n),
        }
    }

    /// The bytes from a place to the end of its block.
    pub fn tail(&self, p: Place) -> Option<&[u8]> {
        match p {
            Place::Global(b, off) => self.global[b].get(off..),
            Place::Script(off) => self.script.get(off..),
            Place::GroupScript(i, off) => self.group_scripts.get(i.checked_sub(1)?)?.get(off..),
            Place::Work(w, off) => self.works.get(w)?.as_ref()?.get(off..),
            Place::Registers(w, off) => self.registers.get(w)?.get(off..),
        }
    }

    /// `n` bytes at a place, read only (none when they run past its block).
    pub fn bytes_at(&self, p: Place, n: usize) -> Option<&[u8]> {
        match p {
            Place::Global(b, off) => self.global[b].get(off..off + n),
            Place::Script(off) => self.script.get(off..off + n),
            Place::GroupScript(i, off) => self.group_scripts.get(i.checked_sub(1)?)?.get(off..off + n),
            Place::Work(w, off) => self.works.get(w)?.as_ref()?.get(off..off + n),
            Place::Registers(w, off) => self.registers.get(w)?.get(off..off + n),
        }
    }

    pub fn read_u32(&mut self, p: Place) -> u32 {
        self.bytes(p, 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn write_u32(&mut self, p: Place, v: u32) {
        if let Some(b) = self.bytes(p, 4) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }

    fn read(&mut self, p: Place, n: usize) -> u32 {
        match self.bytes(p, n) {
            Some(b) => b.iter().rev().fold(0u32, |acc, &x| (acc << 8) | u32::from(x)),
            None => 0,
        }
    }

    fn write(&mut self, p: Place, n: usize, v: u32) {
        if let Some(b) = self.bytes(p, n) {
            for (i, x) in b.iter_mut().enumerate() {
                *x = (v >> (8 * i)) as u8;
            }
        }
    }

    /// Load as an integer (the int path of `push.var`).
    pub fn load_int(&mut self, p: Place, ty: VarType) -> i32 {
        match ty {
            VarType::U8 => self.read(p, 1) as i32,
            VarType::I8 => self.read(p, 1) as u8 as i8 as i32,
            VarType::U16 => self.read(p, 2) as i32,
            VarType::I16 => self.read(p, 2) as u16 as i16 as i32,
            VarType::U32 | VarType::I32 => self.read(p, 4) as i32,
            VarType::F32 => f32::from_bits(self.read(p, 4)) as i32,
            VarType::Bits | VarType::Other(_) => 0,
        }
    }

    /// Load as a float.
    pub fn load_float(&mut self, p: Place, ty: VarType) -> f32 {
        match ty {
            VarType::U8 => self.read(p, 1) as f32,
            VarType::I8 => f32::from(self.read(p, 1) as u8 as i8),
            VarType::U16 => self.read(p, 2) as f32,
            VarType::I16 => f32::from(self.read(p, 2) as u16 as i16),
            // Unsigned 32-bit values convert through a signed int plus 2^32 when negative.
            VarType::U32 => {
                let v = self.read(p, 4) as i32;
                if v < 0 { v as f32 + 4_294_967_296.0 } else { v as f32 }
            }
            VarType::I32 => self.read(p, 4) as i32 as f32,
            VarType::F32 => f32::from_bits(self.read(p, 4)),
            VarType::Bits | VarType::Other(_) => 0.0,
        }
    }

    /// Store an integer; `saturate` clamps to the type's range (the `.b` store variants).
    pub fn store_int(&mut self, p: Place, ty: VarType, v: i32, saturate: bool) {
        let clamp = |lo: i32, hi: i32| if saturate { v.clamp(lo, hi) } else { v };
        match ty {
            VarType::U8 | VarType::Bits => self.write(p, 1, clamp(0, 0xFF) as u32),
            VarType::I8 => self.write(p, 1, clamp(-0x80, 0x7F) as u32),
            VarType::U16 => self.write(p, 2, clamp(0, 0xFFFF) as u32),
            VarType::I16 => self.write(p, 2, clamp(-0x8000, 0x7FFF) as u32),
            VarType::U32 | VarType::I32 => self.write(p, 4, v as u32),
            VarType::F32 => self.write(p, 4, (v as f32).to_bits()),
            VarType::Other(_) => {}
        }
    }

    /// Store a float: converted to an integer first except into float variables.
    pub fn store_float(&mut self, p: Place, ty: VarType, v: f32, saturate: bool) {
        match ty {
            VarType::F32 => self.write(p, 4, v.to_bits()),
            _ => self.store_int(p, ty, v as i32, saturate),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(ty: VarType, area: u8, offset: u32, count: u16) -> Var {
        Var { ty, area, offset, count }
    }

    #[test]
    fn typed_round_trips_and_saturation() {
        let mut m = Memory::new(vec![0; 64], 2);
        let v = var(VarType::I8, 1, 4, 1);
        let p = m.place(&v, 0, 0, 0, [0; 3], 0);
        m.store_int(p, v.ty, 300, false);
        assert_eq!(m.load_int(p, v.ty), 44); // 300 truncated to a byte
        m.store_int(p, v.ty, 300, true);
        assert_eq!(m.load_int(p, v.ty), 127);
        let f = var(VarType::F32, 6, 8, 4);
        let p = m.place(&f, 9, 0, 0, [0; 3], 16); // Index clamps to 3
        assert_eq!(p, Place::Script(16 + 8 + 12));
        m.store_float(p, f.ty, 1.5, false);
        assert_eq!(m.load_float(p, f.ty), 1.5);
        assert_eq!(m.load_int(p, f.ty), 1);
        let u = var(VarType::U32, 1, 0, 1);
        let p = m.place(&u, 0, 0, 0, [0; 3], 0);
        m.store_int(p, u.ty, -1, false);
        assert_eq!(m.load_float(p, u.ty), 4_294_967_295.0);
    }

    #[test]
    fn addresses_round_trip() {
        for p in [
            Place::Global(1, 0x1234),
            Place::Script(0x40),
            Place::GroupScript(3, 0x40),
            Place::Work(9, 0x20),
            Place::Registers(7, 0x10),
        ] {
            assert_eq!(decode_address(encode_address(p)), p);
        }
    }
}
