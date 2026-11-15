//! A small assembler for event scripts, for tests and tools: workers with entries, labels, constants and
//! variables, laid out as an `EV01` file the format reader accepts.

/// One instruction: opcode and optional operand.
#[derive(Debug, Clone, Copy)]
pub struct Op(pub u8, pub Option<u16>);

/// A worker being assembled.
#[derive(Debug, Clone, Default)]
pub struct WorkerSrc {
    pub kind: u8,
    /// Each entry is its own instruction list.
    pub entries: Vec<Vec<Op>>,
    /// Labels as (entry, instruction index).
    pub labels: Vec<(usize, usize)>,
    pub ints: Vec<i32>,
    pub floats: Vec<f32>,
    /// Variable descriptors (type << 28 | area << 25 | offset, count).
    pub vars: Vec<(u32, u16)>,
}

fn put(b: &mut Vec<u8>, at: usize, v: u32) {
    if b.len() < at + 4 {
        b.resize(at + 4, 0);
    }
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Build an `EV01` file. Every worker gets a 0x100-byte private data area per area slot.
pub fn assemble(workers: &[WorkerSrc]) -> Vec<u8> {
    const BASE: usize = 0x40;
    let mut s: Vec<u8> = vec![0; 0x40];
    let n = workers.len();
    // Header, worker descriptor offsets, descriptors, tables, data, then code.
    let desc_at = BASE + 0x38 + 4 * n;
    let mut at = desc_at + 0x34 * n;
    let mut code: Vec<u8> = Vec::new();
    let mut tables = Vec::new();
    for w in workers {
        let mut entry_offs = Vec::new();
        let mut insn_offs: Vec<Vec<u32>> = Vec::new();
        for e in &w.entries {
            entry_offs.push(code.len() as u32);
            let mut offs = Vec::new();
            for op in e {
                offs.push(code.len() as u32);
                match op.1 {
                    Some(v) => {
                        code.push(op.0 | 0x80);
                        code.extend_from_slice(&v.to_le_bytes());
                    }
                    None => code.push(op.0),
                }
            }
            insn_offs.push(offs);
        }
        let labels: Vec<u32> = w.labels.iter().map(|&(e, i)| insn_offs[e][i]).collect();
        tables.push((entry_offs, labels));
    }
    s.resize(at, 0);
    let mut descs = Vec::new();
    for (w, (entries, labels)) in workers.iter().zip(&tables) {
        let mut d = [0u32; 8];
        let mut table = |words: Vec<u32>| {
            let o = at;
            for (i, v) in words.iter().enumerate() {
                put(&mut s, o + 4 * i, *v);
            }
            at += 4 * words.len().max(1);
            s.resize(at, 0);
            o as u32
        };
        let mut vars = Vec::new();
        for &(word, count) in &w.vars {
            vars.push(word);
            vars.push(u32::from(count));
        }
        d[0] = table(vars);
        d[1] = table(w.ints.iter().map(|&v| v as u32).collect());
        d[2] = table(w.floats.iter().map(|v| v.to_bits()).collect());
        d[3] = table(entries.clone());
        d[4] = table(labels.clone());
        for slot in 0..3 {
            d[5 + slot] = at as u32;
            at += 0x100;
            s.resize(at, 0);
        }
        descs.push(d);
    }
    let shared = at as u32;
    at += 0x100;
    let code_at = at;
    s.resize(at, 0);
    s.extend_from_slice(&code);
    let end = s.len() as u32;
    // Script header (relative to BASE).
    let b = BASE;
    put(&mut s, b + 0x20, shared - b as u32);
    put(&mut s, b + 0x2C, end - b as u32);
    put(&mut s, b + 0x30, (code_at - b) as u32);
    s[b + 0x34..b + 0x36].copy_from_slice(&(n as u16).to_le_bytes());
    for (i, (w, d)) in workers.iter().zip(&descs).enumerate() {
        let o = desc_at + 0x34 * i;
        put(&mut s, b + 0x38 + 4 * i, (o - b) as u32);
        s[o] = w.kind;
        s[o + 2..o + 4].copy_from_slice(&(w.vars.len() as u16).to_le_bytes());
        s[o + 4..o + 6].copy_from_slice(&(w.ints.len() as u16).to_le_bytes());
        s[o + 6..o + 8].copy_from_slice(&(w.floats.len() as u16).to_le_bytes());
        s[o + 8..o + 10].copy_from_slice(&(w.entries.len() as u16).to_le_bytes());
        s[o + 10..o + 12].copy_from_slice(&(w.labels.len() as u16).to_le_bytes());
        for (k, field) in [0x14usize, 0x18, 0x1C, 0x20, 0x24, 0x28, 0x2C, 0x30].iter().enumerate() {
            put(&mut s, o + field, d[k] - b as u32);
        }
    }
    s[..4].copy_from_slice(b"EV01");
    put(&mut s, 4, b as u32);
    s
}
