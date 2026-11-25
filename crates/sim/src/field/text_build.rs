//! Natives that build a choice text in a script variable (0140, 0237, 01f3) and show a window with a text from a
//! variable (0141).
//!
//! A built text starts with "\t0", then per entry: a colour switch when the entry's on / off state changes ("\nA"
//! on, "\nB" off), on entries a choice marker ("\x10" and the choice number as a digit), the entry's text, and
//! "\x03" (a line) between entries; 0140 and 0237 end with "\nA" (0237 also copies the text again after its NUL).
//! They return the number of choices << 8 — the window's +0x16 word, which 0141 takes.

use aerugo_script::memory::{Place, decode_address};
use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::{NativeCall, Value};

use super::{FieldState, NAME_SLOT, NAMES, text_space};

/// The bytes of a text address (one handed out by a native, or a script address) up to its NUL.
fn text_at(g: &FieldState, c: &mut NativeCall, addr: u32) -> Vec<u8> {
    if let Some(b) = g.texts.bytes_at(addr) {
        return b.iter().take_while(|&&x| x != 0).copied().collect();
    }
    if (text_space::MACRODIC..=text_space::EMPTY).contains(&addr) {
        return Vec::new();
    }
    c.memory.c_string(decode_address(addr as i32), 0x1000)
}

fn write(c: &mut NativeCall, p: Place, bytes: &[u8]) {
    if let Some(b) = c.memory.bytes_mut(p, bytes.len()) {
        b.copy_from_slice(bytes);
    }
}

/// One entry of a list (0140 / 0237): colour switch, marker, text, line.
fn entry(out: &mut Vec<u8>, prev: &mut i32, count: &mut u8, on: bool, text: &[u8], last: bool) {
    if *prev != i32::from(on) {
        out.extend_from_slice(if on { b"\nA" } else { b"\nB" });
    }
    if on {
        out.extend_from_slice(&[0x10, b'0'.wrapping_add(*count)]);
        *count = count.wrapping_add(1);
    }
    out.extend_from_slice(text);
    if !last {
        out.push(0x03);
    }
    *prev = i32::from(on);
}

pub fn register(n: &mut Natives<FieldState>) {
    // 0237(n, on mask, text address list, out): returns the choices << 8.
    n.register(
        0x0237,
        Native::result(|g, c| {
            let out_v = c.stack.pop_int();
            let list_v = c.stack.pop_int();
            let mask = c.stack.pop_int() as u32;
            let n = c.stack.pop_int();
            let out_p = super::shapes::var_place(g, c, out_v);
            let list_p = super::shapes::var_place(g, c, list_v);
            let mut out = b"\t0".to_vec();
            let (mut prev, mut count) = (-1, 0u8);
            for i in 0..n.max(0) {
                let at = list_p.offset(4 * i as usize);
                let addr = c.memory.read_u32(at);
                let text = text_at(g, c, addr);
                entry(&mut out, &mut prev, &mut count, mask & (1 << (i & 31)) != 0, &text, i >= n - 1);
            }
            out.extend_from_slice(b"\nA");
            let mut all = out.clone();
            all.push(0);
            all.extend_from_slice(&out);
            all.push(0);
            write(c, out_p, &all);
            Value::Int(i32::from(count) << 8)
        }),
    );
    // 0140(table, _, n, on mask, ids, out): the entries are dictionary texts of the table (ids as words, the
    // text id·2, +1 with settings bit 3).
    n.register(
        0x0140,
        Native::result(|g, c| {
            let out_v = c.stack.pop_int();
            let ids_v = c.stack.pop_int();
            let mask = c.stack.pop_int() as u32;
            let n = c.stack.pop_int();
            c.stack.pop_int();
            let table = c.stack.pop_int();
            let out_p = super::shapes::var_place(g, c, out_v);
            let ids_p = super::shapes::var_place(g, c, ids_v);
            let alt = (c.memory.global[0].get(0x3d0c).copied().unwrap_or(0) >> 3 & 1) as i32;
            let mut out = b"\t0".to_vec();
            let (mut prev, mut count) = (-1, 0u8);
            for i in 0..n.max(0) {
                let at = ids_p.offset(4 * i as usize);
                let id = (c.memory.read_u32(at) & 0xffff) as i32;
                let addr = g.texts.macro_text(table, id * 2 + alt).unwrap_or(text_space::EMPTY);
                let text = text_at(g, c, addr);
                entry(&mut out, &mut prev, &mut count, mask & (1 << (i & 31)) != 0, &text, i >= n - 1);
            }
            out.extend_from_slice(b"\nA");
            out.push(0);
            write(c, out_p, &out);
            Value::Int(i32::from(count) << 8)
        }),
    );
    // 01f3(choosable, listed, out): the characters 8–15 in either mask, one per line: "\nB" for listed only, or
    // "\nA" and a choice marker for choosable ones, then the name (save names; character 15 is a kernel text,
    // not kept here). Returns the choices << 8.
    n.register(
        0x01F3,
        Native::result(|g, c| {
            let out_v = c.stack.pop_int();
            let listed = c.stack.pop_int() as u32;
            let choosable = c.stack.pop_int() as u32;
            let out_p = super::shapes::var_place(g, c, out_v);
            let mut out = b"\t0".to_vec();
            let (mut lines, mut count) = (0, 0u8);
            for i in 8..16usize {
                let bit = 1u32 << i;
                if (choosable | listed) & bit == 0 {
                    continue;
                }
                if lines != 0 {
                    out.push(0x03);
                }
                if choosable & bit != 0 {
                    out.extend_from_slice(b"\nA");
                    out.extend_from_slice(&[0x10, b'0'.wrapping_add(count)]);
                    count = count.wrapping_add(1);
                } else {
                    out.extend_from_slice(b"\nB");
                }
                if i < 15 {
                    let at = NAMES + i * NAME_SLOT;
                    let name = c.memory.global[0].get(at..at + NAME_SLOT).unwrap_or_default();
                    out.extend(name.iter().take_while(|&&b| b != 0));
                }
                lines += 1;
            }
            out.push(0);
            write(c, out_p, &out);
            Value::Int(i32::from(count) << 8)
        }),
    );
    // c025(n): event message n becomes character 0's name (copied into save +0x634c) when it is shorter than 0x14
    // bytes (the last message asked = n); returns 1, else 0.
    n.register(
        0xC025,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            g.last_text = n;
            let addr = g.texts.event_text(n);
            let text: Vec<u8> = g
                .texts
                .bytes_at(addr)
                .map_or(Vec::new(), |b| b.iter().take_while(|&&x| x != 0).copied().collect());
            if text.len() < 0x14
                && let Some(dst) = c.memory.global[0].get_mut(NAMES..NAMES + text.len() + 1)
            {
                dst[..text.len()].copy_from_slice(&text);
                dst[text.len()] = 0;
                return Value::Int(1);
            }
            Value::Int(0)
        }),
    );
    // 0141(window, word, text variable): the window shows the variable's text (+8/+0xc its address, +0x16 the
    // word: the choices in its high byte); returns 0.
    n.register(
        0x0141,
        Native::result(|g, c| {
            let var = c.stack.pop_int();
            let word = c.stack.pop_int();
            let w = c.stack.pop_int().clamp(0, 7) as usize;
            let p = super::shapes::var_place(g, c, var);
            let bytes = c.memory.c_string(p, 0x1000);
            if let Some(cs) = g.charset.clone() {
                g.messages.push(super::message_from_bytes(bytes, word as u16, &cs));
                let m = g.messages.len() - 1;
                if let Some(win) = g.windows.get_mut(w) {
                    win.message = Some(m);
                    win.entry_counts = word as u16;
                }
            }
            Value::Int(0)
        }),
    );
}
