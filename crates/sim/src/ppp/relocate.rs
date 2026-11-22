//! Turning an effect file's offsets into addresses, as the game does on first use: the OEF2 directory's tables
//! (its set-up), then a particle group's lists and each part's chain of sub-part definitions, their track and
//! work references and the program references into the effect's program table (the bind).

use super::Space;

/// The OEF2 tables: (offset field, count field, record stride, field rewritten within a record).
const TABLES: [(u32, u32, u32, u32); 7] = [
    (4, 0x42, 4, 0),
    (8, 0x44, 0x20, 0x14),
    (0xc, 0x46, 0x10, 4),
    (0x10, 0x48, 4, 0),
    (0x14, 0x4a, 4, 0),
    (0x20, 0x50, 4, 0),
    (0x24, 0x52, 4, 0),
];

/// The OEF2 set-up for the file at `file` (its second header at file + [file + 0x3c]): once (the second
/// header's +0x40 flag), each entry of the second header's tables becomes an address in the directory's table.
pub fn oef2_set(m: &mut Space, file: u32) {
    let second = file.wrapping_add(m.u32(file + 0x3c));
    if m.u16(second + 0x40) != 0 {
        return;
    }
    m.set_u16(second + 0x40, 1);
    for (off, cnt, stride, field) in TABLES {
        let n = u32::from(m.u16(second + cnt));
        let src = second.wrapping_add(m.u32(second + off));
        let dst = file.wrapping_add(m.u32(file + off)) + field;
        for i in 0..n {
            let v = m.u32(src + 4 * i);
            if v != 0 {
                m.set_u32(dst + i * stride, v.wrapping_add(second));
            }
        }
    }
    // The 0x10-byte records of +0x18: kind 1 records keep a size at +0xc that becomes the end address, or
    // (negative) take an earlier record's by index.
    let n = u32::from(m.u16(second + 0x4c));
    let src = second.wrapping_add(m.u32(second + 0x18));
    let recs = file.wrapping_add(m.u32(file + 0x18));
    for i in 0..n {
        let v = m.u32(src + 4 * i);
        let r = recs + i * 0x10;
        let size = m.u32(r + 8);
        if v != 0 && (size as i32) < 0x10_0000 {
            let at = v.wrapping_add(second);
            m.set_u32(r + 8, at);
            if m.u8(r) == 1 && (m.u32(r + 0xc) as i32) > 0 {
                m.set_u32(r + 0xc, at.wrapping_add(size));
            }
        }
    }
    for i in 0..n {
        let r = recs + i * 0x10;
        let v = m.u32(r + 0xc);
        if m.u8(r) == 1 && (v as i32) < 0 && v != 0x8000_0000 {
            let from = m.u32(recs + (v & 0xff) * 0x10 + 0xc);
            m.set_u32(r + 0xc, from);
        }
    }
}

/// The getter for the directory (kind 8: particle group `n`, through the +0x20 table).
pub fn oef2_particle(m: &mut Space, file: u32, n: u32) -> u32 {
    let table = file.wrapping_add(m.u32(file + 0x20));
    m.u32(table + 4 * n)
}

/// Bind the particle group at `group` to the program table at `table` (0x28-byte entries).
pub fn bind(m: &mut Space, group: u32, table: u32) {
    for f in [0x10, 0x14, 0x18, 0x1c] {
        let v = m.u32(group + f);
        m.set_u32(group + f, v.wrapping_add(group));
    }
    let parts = m.u32(group + 0x10);
    for i in 0..u32::from(m.u16(group + 6)) {
        let at = parts + 4 * i;
        let rec = m.u32(at).wrapping_add(group);
        m.set_u32(at, rec);
        bind_part(m, rec, table);
    }
    let recs = m.u32(group + 0x14);
    for i in 0..u32::from(m.u16(group + 8)) {
        for f in [0x14, 0x18, 0x1c] {
            let at = recs + i * 0x20 + f;
            let v = m.u32(at);
            m.set_u32(at, v.wrapping_add(group));
        }
    }
    let list = m.u32(group + 0x18);
    for i in 0..u32::from(m.u16(group + 0xa)) {
        let v = m.u32(list + 4 * i);
        m.set_u32(list + 4 * i, v.wrapping_add(group));
    }
    let pairs = m.u32(group + 0x1c);
    for i in 0..u32::from(m.u16(group + 0xc)) {
        let at = pairs + i * 8 + 4;
        let v = m.u32(at);
        m.set_u32(at, v.wrapping_add(group));
    }
}

/// One part record: its definitions' chain (the terminator cut off), each definition's references (program,
/// track, work pair), its program list and its second list.
fn bind_part(m: &mut Space, rec: u32, table: u32) {
    let programs = rec.wrapping_add(m.u32(rec + 8));
    let second = rec.wrapping_add(m.u32(rec + 0xc));
    let (np, ns) = (m.u32(programs) as i32, m.u32(second) as i32);
    let mut def = rec + 0x10;
    if m.u32(def) == 0 {
        return;
    }
    loop {
        let next = m.u32(def).wrapping_add(rec);
        m.set_u32(def, next);
        let refs = m.u16(def + 0x26) as i16;
        for k in 0..refs.max(0) as u32 {
            let rf = def + 0x28 + k * 0x10;
            let idx = m.u32(rf);
            m.set_u32(rf, table.wrapping_add(idx.wrapping_mul(0x28)));
            let t = m.u32(rf + 8);
            m.set_u32(rf + 8, t.wrapping_add(rec));
            let w = m.u32(rf + 0xc);
            m.set_u32(rf + 0xc, w.wrapping_add(rec));
        }
        if m.u32(next) == 0 {
            break;
        }
        def = next;
    }
    m.set_u32(def, 0);
    for i in 0..np.max(0) as u32 {
        let at = programs + 4 * (i + 1);
        let idx = m.u32(at);
        m.set_u32(at, table.wrapping_add(idx.wrapping_mul(0x28)));
    }
    for i in 0..ns.max(0) as u32 {
        let at = second + 4 * (i + 1);
        let v = m.u32(at);
        m.set_u32(at, v.wrapping_add(rec));
    }
}
