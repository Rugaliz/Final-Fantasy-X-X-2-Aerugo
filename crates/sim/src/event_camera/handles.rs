//! Camera handles: a handle is owner << 24 | kind << 16 | sub-camera << 8 | camera record. A record has four
//! sub-camera slots (record +0xc68 + sub·8: handle, suspended, held, priority adjust); the active sub-camera is
//! the one with the highest priority (its kind plus the adjust) that is neither suspended nor held.

use super::{CamMem, FILTER, HANDLES_K1, HANDLES_K3, HANDLES_K5, HANDLES_K7, RECORD, RECORDS};

/// Sub-camera slot `sub` of record `r`.
fn slot(r: u32, sub: u32) -> u32 {
    RECORDS + r * RECORD + 0xc68 + sub * 8
}

impl CamMem {
    /// The active sub-camera of a record: the highest priority among the slots that
    /// are neither suspended nor held (ties keep the earlier slot).
    pub fn active_sub(&self, r: u32) -> u32 {
        let prio = |sub: u32| {
            let s = slot(r, sub);
            i32::from(self.u16(s + 2) & 0xff) + i32::from(self.i8(s + 6))
        };
        let free = |sub: u32| self.u8(slot(r, sub) + 4) == 0 && self.u8(slot(r, sub) + 5) == 0;
        let mut best = 0;
        let mut top = 0;
        if prio(0) > 0 && free(0) {
            top = prio(0);
        }
        for sub in 1..3 {
            if top < prio(sub) && free(sub) {
                best = sub;
                top = prio(sub);
            }
        }
        if prio(3) <= top || !free(3) { best } else { 3 }
    }

    /// The handle of a record's active sub-camera, or 0 when it is suspended or held.
    pub fn active_handle(&self, r: u32) -> u32 {
        let s = slot(r, self.active_sub(r));
        if self.u8(s + 4) == 0 && self.u8(s + 5) == 0 { self.u32(s) } else { 0 }
    }

    /// The handle is live and may set filters: a handle without a kind asks
    /// whether any slot of its record is in use and not held.
    pub fn filter_awake(&self, h: u32) -> bool {
        let r = h & 0xff;
        (0..4).any(|sub| {
            let s = slot(r, sub);
            if h & 0xff_0000 == 0 {
                self.u8(s + 2) != 0 && self.u8(s + 5) == 0
            } else {
                self.u32(s) == h && self.u8(s + 5) == 0
            }
        })
    }

    /// Get (or open) a sub-camera of record `r` for an owner and kind. A new one takes the
    /// first free slot; when it is not the active one it starts from the active one's roll and screen distance
    /// targets and filters (their easing and limit counts cleared).
    pub fn camera_get(&mut self, owner: u32, r: u32, kind: u32) -> u32 {
        let kind = kind & 0xf;
        if kind == 0 || r > 2 {
            return 0;
        }
        for sub in 0..4 {
            let h = self.u32(slot(r, sub));
            if owner == h >> 24 && kind == (h >> 16) & 0xf && r == h & 0xff {
                return h;
            }
        }
        let rec = RECORDS + r * RECORD;
        for sub in 0..4 {
            if self.u32(slot(r, sub)) & 0xf_0000 != 0 {
                continue;
            }
            let active = self.active_sub(r);
            let h = ((owner * 0x100 + kind) * 0x100 + sub) * 0x100 + r;
            self.set_u32(slot(r, sub), h);
            self.set_u8(slot(r, sub) + 4, 0);
            self.set_u8(slot(r, sub) + 6, 0);
            if active != sub {
                self.copy(rec + 0xcb8 + sub * 0x30, rec + 0xcb8 + active * 0x30, 0x30);
                for side in 0..2u32 {
                    let f = rec + 0x30 + side * super::SIDE + 0xc0;
                    let (dst, src) = (f + sub * FILTER, f + active * FILTER);
                    self.copy(dst, src, FILTER);
                    self.set_vec3(dst + 0x70, [0.0; 3]);
                    self.set_u8(dst + 4, 0);
                    self.set_f32(dst + 0x7c, 0.0);
                }
            }
            return h;
        }
        0
    }

    /// A worker's camera handle: `slot` 0 kind 3, 1 kind 1 (the event's camera
    /// worker), 2 kind 5, 3 kind 7, cached per camera record.
    pub fn get_handle(&mut self, owner: u32, which: u32, r: u32) -> u32 {
        if r >= 3 {
            return 0;
        }
        let (table, kind) = match which {
            0 => (HANDLES_K3, 3),
            1 => (HANDLES_K1, 1),
            2 => (HANDLES_K5, 5),
            3 => (HANDLES_K7, 7),
            _ => return 0,
        };
        let a = table + r * 4;
        if self.u32(a) == 0 {
            let h = self.camera_get(owner, r, kind);
            self.set_u32(a, h);
        }
        self.u32(a)
    }

    /// Free the slots holding a handle (the kind's high nibble is ignored); returns how many.
    pub fn free_handle(&mut self, h: u32) -> u32 {
        let r = h & 0xff;
        let key = h & 0xff0f_ffff;
        let mut n = 0;
        for sub in 0..4 {
            let s = slot(r, sub);
            if self.u32(s) & 0xff0f_ffff == key {
                self.set_u32(s, 0);
                self.set_u16(s + 4, 0);
                self.set_u8(s + 6, 0);
                n += 1;
            }
        }
        n
    }

    /// A worker lets go of its camera: the handle leaves the tables and its slots.
    pub fn bind_off(&mut self, h: u32) {
        if h == 0 {
            return;
        }
        for table in [HANDLES_K1, HANDLES_K3, HANDLES_K5, HANDLES_K7] {
            for r in 0..3 {
                if self.u32(table + r * 4) == h {
                    self.set_u32(table + r * 4, 0);
                }
            }
        }
        self.free_handle(h);
    }

    /// Suspend (1) or resume (0) every slot holding the handle.
    pub fn set_suspended(&mut self, h: u32, v: u8) {
        for sub in 0..4 {
            let s = slot(h & 0xff, sub);
            if self.u32(s) == h {
                self.set_u8(s + 4, v & 1);
            }
        }
    }
}
