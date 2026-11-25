//! The turn order as the battle frame runs it (790fb0): while nobody waits for a command, every CTB tick takes
//! one off each active record's wait; whoever reaches 0 is ready, the ready are sorted (characters by agility,
//! highest first, then by slot; monsters after them by slot) and the first joins the input cue (7b2430), which
//! holds the turn order until its command is chosen and run (792a90, MsBtlReadManage).

use super::Battle;
use super::records::{self as r, Fields};

/// Status word bit 2: the record's wait does not run (and it takes no turn).
const STATUS_WAIT_HELD: u16 = 4;
/// The input cue's size (DAT 112aa80, 8 bytes an entry).
const INPUT_CUE_SIZE: usize = 0x3e;

/// One input cue entry (7b2430's arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputCueEntry {
    pub slot: u8,
    /// +1, +2 and +4 of the entry (7b2430's fourth, third and second arguments).
    pub kind: u8,
    pub turn: u8,
    pub extra: u8,
    /// +3: its command choice has started (792a90).
    pub started: bool,
}

/// The turn state the frame keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turns {
    /// The input cue (DAT bde0 entries at 112aa80).
    pub input_cue: Vec<InputCueEntry>,
    /// Frames since the last tick (DAT bde2) and frames a tick takes (DAT bde3, 1 from MsBattleInit).
    pub frames: u8,
    pub frames_per_tick: u8,
    /// The turn order is held (DAT a8e1).
    pub held: bool,
    /// The debug switch "CTB Same Order" (DAT a8fb): waits stop at 1.
    pub same_order: bool,
    /// The turns taken so far (for the host), in order.
    pub history: Vec<u8>,
}

impl Default for Turns {
    fn default() -> Self {
        Turns {
            input_cue: Vec::new(),
            frames: 0,
            frames_per_tick: 1,
            held: false,
            same_order: false,
            history: Vec::new(),
        }
    }
}

/// 0078f000: the ready list's sort key.
fn turn_key(b: &Battle, slot: usize) -> i32 {
    if r::is_monster_slot(slot) {
        slot as i32 + 0x10000
    } else {
        (0xff - i32::from(b.records.get(slot).u8_at(r::AGILITY))) * 0x100 + slot as i32
    }
}

/// Whether a record's wait runs: on the field, not fallen, not held by its status, not already in the cue.
fn waits(rec: &[u8]) -> bool {
    rec.u8_at(r::IN_CUE) == 0
        && rec.u8_at(r::ACTIVE) != 0
        && rec.u8_at(r::DEATH_STATE) == 0
        && rec.u16_at(r::STATUS) & STATUS_WAIT_HELD == 0
}

/// 790fb0: one frame of the turn order. Returns the slot that joined the input cue, if any.
pub fn frame(b: &mut Battle) -> Option<u8> {
    if b.turns.held || !b.turns.input_cue.is_empty() {
        return None;
    }
    // The ready (DAT 11333c0): the records whose wait is over and that take turns. (The original holds
    // everything while a few effects and commands play with a message counter running; no battle effect runs
    // here yet.)
    let mut ready: Vec<u8> = (0..r::FULL_SLOTS)
        .filter(|&s| {
            let rec = b.records.get(s);
            waits(rec) && rec.u8_at(r::CTB) == 0 && rec.u8_at(r::TAKES_TURNS) != 0
        })
        .map(|s| s as u8)
        .collect();
    if ready.is_empty() {
        b.turns.frames = b.turns.frames.wrapping_add(1);
        if b.turns.frames >= b.turns.frames_per_tick {
            b.turns.frames = 0;
            tick(b);
        }
        return None;
    }
    // 0078d3e0: a selection sort on the key (the first of equal keys stays first).
    for i in 0..ready.len().saturating_sub(1) {
        let mut best = i;
        for k in i + 1..ready.len() {
            if turn_key(b, usize::from(ready[k])) < turn_key(b, usize::from(ready[best])) {
                best = k;
            }
        }
        ready.swap(i, best);
    }
    let slot = ready[0];
    if join_input_cue(b, slot, 0, 1, 0) {
        log::trace!("battle turn: slot {slot:#x}'s turn-start overdrive charges (7b13c0) are not ported");
        b.turns.history.push(slot);
        return Some(slot);
    }
    None
}

/// One CTB tick: every waiting record's wait goes down by one; at 0 it is marked (+0xde8 = 3) and a record that
/// takes no turns starts over at a full turn.
fn tick(b: &mut Battle) {
    let same_order = b.turns.same_order;
    for s in 0..r::FULL_SLOTS {
        let rec = b.records.get_mut(s);
        if !waits(rec) {
            continue;
        }
        let ticks = rec.u8_at(r::TICKS);
        if ticks != 0xff {
            rec.set_u8(r::TICKS, ticks.wrapping_add(1));
        }
        let mut wait = i32::from(rec.u8_at(r::CTB)) - 1;
        if wait < 1 {
            wait = 0;
            rec.set_u8(r::TURN_MARK, 3);
            if rec.u8_at(r::TAKES_TURNS) == 0 {
                wait = i32::from(rec.u8_at(r::CTB_TURN));
            }
        } else if same_order {
            wait = 1;
        }
        rec.set_u8(r::CTB, wait as u8);
    }
}

/// 7b2430: a record joins the input cue.
pub fn join_input_cue(b: &mut Battle, slot: u8, extra: u8, turn: u8, kind: u8) -> bool {
    if b.turns.input_cue.len() >= INPUT_CUE_SIZE {
        return false;
    }
    let rec = b.records.get_mut(usize::from(slot));
    let n = rec.u8_at(r::IN_CUE).wrapping_add(1);
    rec.set_u8(r::IN_CUE, n);
    b.turns.input_cue.push(InputCueEntry { slot, kind, turn, extra, started: false });
    true
}
