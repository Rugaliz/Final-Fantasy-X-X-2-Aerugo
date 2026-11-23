//! Map changes the scripts ask for (0011 / 00ab with the save's fade, 010b / 010c without).
//!
//! The request only stands while the system block's byte 0 has bit 7 (the block's setup sets it; a request
//! clears it, 00be sets it again). It turns the player's control off, moves the save's map and entrance to the
//! previous ones and the target's into their place (a negative target returns to the place the save keeps at
//! +0xb8), turns bit 7 off and bit 1 of the +0x210 word on, starts the positional sounds' fade out (−1/15 a
//! frame) and sets the frames to wait: 0011 the save's +0x12 frames with a fade to black (+0x11 = 0) or white
//! (1); 010b none (30 on map 0x109). Each frame before the scripts the wait counts down, then one more frame
//! when there was a wait, then the next scene is set up (the remaster shows its loading screen first, except
//! for scenes 0x17, 0x15c and 0x10c at story 0x1a9).

use aerugo_script::vm::NativeCall;

use super::FieldState;

/// The map the original's map change treats as the software reset (the title).
const RESET_TARGET: i32 = 399;

/// A map change under way: the frames left and the one extra frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapChange {
    pub frames: i32,
    pub extra: i32,
}

/// The scene the map change sets up next: the save's map and entrance, and whether the remaster's loading
/// screen comes up for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NextScene {
    pub map: i32,
    pub entrance: i32,
    pub loading_screen: bool,
}

fn u16_at(save: &[u8], at: usize) -> u16 {
    save.get(at..at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

fn set_u16(save: &mut [u8], at: usize, v: u16) {
    if let Some(b) = save.get_mut(at..at + 2) {
        b.copy_from_slice(&v.to_le_bytes());
    }
}

fn byte(save: &[u8], at: usize) -> u8 {
    save.get(at).copied().unwrap_or(0)
}

fn set_byte(save: &mut [u8], at: usize, v: u8) {
    if let Some(b) = save.get_mut(at) {
        *b = v;
    }
}

/// The save's map and entrance for the change (the original's 88ea60).
fn shift_save(g: &mut FieldState, save: &mut [u8], target: i32, entrance: i8) {
    if target == RESET_TARGET {
        log::error!("GAP: map change to the software reset target {RESET_TARGET} (soft reset not ported)");
        g.soft_reset = true;
        return;
    }
    if target == 0x17 {
        log::error!("GAP: map change to map 0x17: its scene release (645310) is not ported");
    }
    let (map, entrance) = if target < 0 {
        let back = u16_at(save, 0xb8);
        set_u16(save, 0xb8, 0);
        (u16_at(save, 0xba), back as u8)
    } else {
        (target as u16, entrance as u8)
    };
    set_byte(save, 0xd, byte(save, 0xc));
    set_u16(save, 2, u16_at(save, 0));
    set_u16(save, 0, map);
    set_byte(save, 0xc, entrance);
    if target < 0 {
        set_u16(save, 2, u16_at(save, 0xc2));
        set_byte(save, 0xd, byte(save, 0xce));
    }
    g.map_change = Some(MapChange { frames: 0, extra: 0 });
}

/// A map change request (the original's 86ff40): `with_fade` for 0011 / 00ab.
pub fn request(g: &mut FieldState, c: &mut NativeCall, target: i32, entrance: i8, with_fade: bool) {
    // The original's other gate is its debug switch, off in a release build.
    if g.workers.sys.flags0 & 0x80 == 0 {
        return;
    }
    g.control_off();
    let save = &mut c.memory.global[0];
    shift_save(g, save, target, entrance);
    // A battle asked for but not started is dropped (781530; its keep switch is a debug setting).
    if matches!(g.battle, Some((_, false))) {
        g.battle = None;
    }
    g.workers.sys.flags0 &= 0x7f;
    g.workers.sys.flags210 |= 2;
    g.pos_sounds.rate = f32::from_bits(0xbd88_8889);
    let frames = if with_fade {
        let frames = byte(save, 0x12);
        let white = match byte(save, 0x11) {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        if let Some(white) = white {
            g.screen.fade(i16::from(frames), false, white);
        }
        i32::from(frames)
    } else if u16_at(save, 2) == 0x109 {
        0x1e
    } else {
        0
    };
    if let Some(m) = &mut g.map_change {
        *m = MapChange { frames, extra: frames };
    }
}

/// The frame's check before the scripts: the wait counts down, then the next scene is due.
pub fn frame(g: &mut FieldState, save: &[u8]) -> Option<NextScene> {
    let m = g.map_change.as_mut()?;
    if m.frames >= 1 {
        m.frames -= 1;
        return None;
    }
    if m.extra >= 1 {
        m.extra = 0;
        return None;
    }
    g.map_change = None;
    let map = i32::from(u16_at(save, 0));
    let story = u16_at(save, 0xbec);
    let loading_screen = !(map == 0x17 || map == 0x15c || (map == 0x10c && story == 0x1a9));
    Some(NextScene { map, entrance: i32::from(byte(save, 0xc)), loading_screen })
}
