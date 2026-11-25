//! Commands: the choice for the combatant at the head of the input cue (792a90), the command cue it fills
//! (MsSetBtlCommand), the action scripts every turn runs (MsActionRequest), the command run (MsBtlReadManage)
//! and the turn's end (MsDeleteInputCue: the next wait is the command's rank × the record's turn ticks).
//!
//! A command record is 0x48 bytes: +0 the slot, +1 the cue kind (0 or 1), +3 set (an action script filled it),
//! +4 the run state, +8 the command ids (u16), +0x10 + 0x10·n the targets of the n-th id.

use super::Battle;
use super::records::{self as r, Fields};
use super::scripts::{self, BattleScripts, Target};

pub const COMMAND_SIZE: usize = 0x48;
pub type CommandRecord = [u8; COMMAND_SIZE];

mod command {
    pub const SLOT: usize = 0;
    pub const KIND: usize = 1;
    pub const SET: usize = 3;
    pub const STATE: usize = 4;
}

/// The command cue's size (DAT 112ac70, bde1 records).
const COMMAND_CUE_SIZE: usize = 0x3e;

/// The battle's command state.
#[derive(Debug, Clone)]
pub struct Commands {
    /// The command cue (DAT 112ac70).
    pub cue: Vec<CommandRecord>,
    /// The record the action scripts fill (DAT 1136a78).
    pub from_scripts: CommandRecord,
    /// A character waits for the player's command (the menu, 89bb10).
    pub awaiting_input: Option<u8>,
    /// What 7928d0 last found: 0 the battle goes on, 1 the party is gone, 3 none of it on the field, 4 / 2 the
    /// monsters are gone.
    pub outcome: u8,
}

impl Default for Commands {
    fn default() -> Self {
        Commands { cue: Vec::new(), from_scripts: [0; COMMAND_SIZE], awaiting_input: None, outcome: 0 }
    }
}

/// Turn-entry numbers the action scripts run (MsActionRequest's third argument).
pub mod action {
    /// The command itself (a counter runs entry 3).
    pub const COMMAND: usize = 0;
    pub const TURN_START: usize = 1;
    pub const COUNTER: usize = 3;
    pub const TURN_END: usize = 6;
    /// An aeon's turn start.
    pub const AEON_TURN_START: usize = 0xc;
}

/// The battle event entry each turn starts with (797bc0 space 0).
const TURN_EVENT: usize = 0x3e;

/// 7928d0: whether the battle goes on.
pub fn outcome(b: &Battle) -> u8 {
    let (mut standing, mut on_field) = (0, 0);
    for &slot in &b.front_line {
        if slot == 0xff {
            continue;
        }
        let rec = b.records.get(usize::from(slot));
        if rec.u8_at(r::COUNTS) != 0
            && rec.u8_at(r::COUNTS + 1) != 0
            && rec.i32_at(r::HP) > 0
            && rec.u8_at(r::DEATH_STATE) == 0
            && rec.u8_at(r::FALLING) == 0
        {
            standing += 1;
            if rec.u8_at(r::ACTIVE) != 0 {
                on_field += 1;
            }
        }
    }
    if standing == 0 {
        return 1;
    }
    if on_field == 0 {
        return 3;
    }
    let mut all_falling = true;
    for i in 0..r::MONSTERS {
        let rec = b.records.monster(i);
        if rec.u8_at(r::ACTIVE) != 0 && rec.u8_at(r::COUNTS) != 0 && rec.u8_at(r::DEATH_STATE) == 0 {
            all_falling &= rec.u8_at(r::FALLING) != 0;
        }
    }
    if all_falling {
        log::trace!("battle: the monsters are gone (DAT c9eb, which picks 2 over 4, is not tracked)");
        4
    } else {
        0
    }
}

/// MsActionRequest: run a combatant's action entry at once (a character's from the battle's script, space
/// slot + 0x29; an aeon with `aeon_space` from 0x25–0x28; a monster's from its own script, space 0x3d). Returns
/// whether a script ran it. What the scripts put in the command record is handled by the caller.
pub fn action_request(
    scripts: &mut BattleScripts,
    b: &mut Battle,
    globals: &mut [Vec<u8>; 2],
    slot: u8,
    by: u8,
    entry: usize,
    aeon_space: bool,
) -> bool {
    if by != 0xff {
        b.records.get_mut(usize::from(slot)).set_u8(r::LAST_ATTACKER, by);
    }
    log::trace!("battle action {entry} for slot {slot:#x}: 7acb00 and 7979d0 are not run");
    let worker = if r::is_monster_slot(usize::from(slot)) {
        let t = Target::Monster(usize::from(slot));
        scripts::request_exe(scripts, b, t, 0x3d, &[(t, 0x3d, entry)])
    } else {
        let aeon = match (aeon_space, slot) {
            (true, 0xe..=0x11) => Some(usize::from(slot) + 0x17),
            _ => None,
        };
        match aeon {
            None => {
                let space = usize::from(slot) + 0x29;
                scripts::request_exe(
                    scripts,
                    b,
                    Target::BattleScript(1),
                    space,
                    &[(Target::BattleScript(1), space, entry)],
                )
            }
            Some(space) => scripts::request_exe(
                scripts,
                b,
                Target::BattleScript(0),
                space,
                &[(Target::BattleScript(1), space, entry), (Target::BattleScript(0), space, entry)],
            ),
        }
    };
    match worker {
        Some(w) => {
            scripts::run_now(scripts, b, globals, w);
            true
        }
        None => false,
    }
}

/// MsSetBtlCommand: a command joins the command cue.
fn set_command(b: &mut Battle, slot: u8, record: &CommandRecord, kind: u8) -> bool {
    if b.commands.cue.len() >= COMMAND_CUE_SIZE {
        return false;
    }
    let rec = b.records.get_mut(usize::from(slot));
    let n = rec.u8_at(r::IN_COMMAND_CUE).wrapping_add(1);
    rec.set_u8(r::IN_COMMAND_CUE, n);
    let mut c = *record;
    c[command::SLOT] = slot;
    c[command::KIND] = kind;
    c[2] = 0;
    c[command::STATE] = 0;
    c[5] = 0;
    c[6] = 0;
    c[7] = 0;
    b.commands.cue.push(c);
    log::trace!("battle: MsUseItem for the new command is not run");
    true
}

/// 792d60: queue a command for its slot (or drop the slot's turn when the cue is full).
pub fn queue_command(b: &mut Battle, record: &CommandRecord, kind: u8) {
    let slot = record[command::SLOT];
    if usize::from(slot) < r::FULL_SLOTS && kind <= 1 && !set_command(b, slot, record, kind) {
        end_turn(b, slot);
    }
}

/// 792a90: the command choice for the head of the input cue.
pub fn choose(scripts: &mut BattleScripts, b: &mut Battle, globals: &mut [Vec<u8>; 2]) {
    loop {
        let Some(&head) = b.turns.input_cue.first() else { return };
        if head.started {
            return;
        }
        let slot = head.slot;
        log::trace!("battle: MsCheckStatusBeforeAction for slot {slot:#x} is not run");
        b.commands.outcome = outcome(b);
        if b.records.get(usize::from(slot)).u8_at(r::DEATH_STATE) != 0 || b.commands.outcome != 0 {
            end_turn(b, slot);
            continue;
        }
        b.globals.chosen = slot;
        if scripts::request(scripts, b, 0, Some(usize::from(slot)), TURN_EVENT).is_none() {
            log::trace!("battle: no script has the turn event for slot {slot:#x}");
        }
        if b.records.get(usize::from(slot)).u8_at(r::TURN_STARTED) == 0 {
            b.records.get_mut(usize::from(slot)).set_u8(r::TURN_STARTED, 1);
            action_request(scripts, b, globals, slot, 0xff, action::TURN_START, false);
            if is_aeon(b, slot) {
                action_request(scripts, b, globals, slot, 0xff, action::AEON_TURN_START, true);
            }
        }
        b.turns.input_cue[0].started = true;
        let rec = b.records.get_mut(usize::from(slot));
        rec.set_u8(r::INPUT_INDEX, 0);
        rec.set_u8(r::TURN_STARTED, 0);
        let mut record: CommandRecord = [0; COMMAND_SIZE];
        record[command::SLOT] = slot;
        let rec = b.records.get(usize::from(slot));
        let (status, counter, monster) =
            (rec.u16_at(r::STATUS), rec.u8_at(r::STATUS_COUNTERS), rec.u8_at(r::FORMATION_MONSTER) != 0);
        if rec.u16_at(r::STATUS2) & 0x4000 != 0 && rec.u8_at(r::AUTO_COMMAND_DONE) == 0 {
            log::error!("GAP: battle auto-command (799cd0) is not ported");
        } else if counter != 0 {
            // Under the first status counter the record queues an empty command its scripts fill.
            queue_command(b, &record, head.kind);
        } else if status & 0x400 != 0 {
            log::error!("GAP: battle command under status bit 10 (MsGetComInfo) is not ported");
        } else if status & 0x300 != 0 {
            log::error!("GAP: battle berserk commands (MsSetBerserkCommand) are not ported");
        } else if monster {
            // 792120: a formation monster's command comes from its action scripts when it runs (slot 0xe's
            // random rule does not apply to monster slots).
            queue_command(b, &record, head.kind);
        } else {
            if slot == 0xe {
                log::error!("GAP: battle slot 0xe's random command rule (792120, stream 0x11) is not ported");
            }
            b.commands.awaiting_input = Some(slot);
        }
        b.records.get_mut(usize::from(slot)).set_u8(r::AUTO_COMMAND_DONE, 0);
        return;
    }
}

fn is_aeon(b: &Battle, slot: u8) -> bool {
    b.records.get(usize::from(slot)).u8_at(r::ROM_FLAGS) >> 2 & 1 != 0
}

/// The player's command for the character waiting (what the menu returns): `command` with `targets`.
pub fn player_command(b: &mut Battle, command: u16, targets: u32) {
    let Some(slot) = b.commands.awaiting_input.take() else { return };
    let mut record: CommandRecord = [0; COMMAND_SIZE];
    record[command::SLOT] = slot;
    record.set_u16(8, command);
    record.set_u16(10, 0xff);
    record.set_u32(0x10, targets);
    let kind = b.turns.input_cue.first().map_or(0, |e| e.kind);
    queue_command(b, &record, kind);
}

/// MsBtlReadManage, so far: the head command runs its action scripts (the command entry) and ends the turn; the
/// command's effect (MsCalcMagic, MsParseCommand, MsCalcCommand) is not ported.
pub fn run(scripts: &mut BattleScripts, b: &mut Battle, globals: &mut [Vec<u8>; 2]) {
    let Some(head) = b.commands.cue.first().copied() else { return };
    let slot = head[command::SLOT];
    let rec = b.records.get(usize::from(slot));
    if rec.u8_at(r::APPEARING) != 0 || rec.u8_at(r::COMMAND_HELD) != 0 {
        return;
    }
    b.commands.from_scripts = [0; COMMAND_SIZE];
    let entry = if head[command::KIND] == 0 { action::COMMAND } else { action::COUNTER };
    b.globals.actor = slot;
    b.globals.chosen = slot;
    action_request(scripts, b, globals, slot, 0xff, entry, true);
    let filled = b.commands.from_scripts;
    if filled[command::SET] != 0 {
        log::trace!("battle: slot {slot:#x}'s scripts chose command {:#06x}", filled.u16_at(8));
    }
    if !b.globals.command_reported {
        b.globals.command_reported = true;
        log::error!(
            "GAP: battle command effects (MsCalcMagic, MsParseCommand, MsCalcCommand, damage) are not ported"
        );
    }
    b.commands.cue.remove(0);
    let rec = b.records.get_mut(usize::from(slot));
    let n = rec.u8_at(r::IN_COMMAND_CUE).saturating_sub(1);
    rec.set_u8(r::IN_COMMAND_CUE, n);
    end_turn(b, slot);
    action_request(scripts, b, globals, slot, 0xff, action::TURN_END, false);
}

/// MsDeleteInputCue for the slot's turn: out of the input cue, and its next wait: the command's rank
/// (+0xde8) × its turn ticks, halved and doubled by its status counters.
pub fn end_turn(b: &mut Battle, slot: u8) {
    let Some(at) = b.turns.input_cue.iter().position(|e| e.slot == slot) else { return };
    let entry = b.turns.input_cue.remove(at);
    let ticks = {
        let agility = b.records.get(usize::from(slot)).u8_at(r::AGILITY);
        let index = r::clamp(i32::from(agility), 1, 0xff) - 1;
        b.tables
            .as_ref()
            .and_then(|t| super::kernel_record(&t.ctb_base, index as u32))
            .map_or(0, |k| k.bytes.u8_at(0))
    };
    let rec = b.records.get_mut(usize::from(slot));
    let n = rec.u8_at(r::IN_CUE).saturating_sub(1);
    rec.set_u8(r::IN_CUE, n);
    if rec.u8_at(r::INPUT_INDEX) == 0 {
        rec.set_u8(r::INPUT_INDEX, 0xff);
    }
    if entry.kind == 0 {
        let rank = i32::from(rec.u8_at(r::TURN_MARK)).max(1);
        let mut wait = i32::from(ticks) * rank;
        if rec.u8_at(r::HALVES_WAIT) != 0 {
            wait /= 2;
        }
        if rec.u8_at(r::DOUBLES_WAIT) != 0 {
            wait *= 2;
        }
        let cost = r::clamp(wait, 0, 0xff) as u8;
        let v = rec.u8_at(r::CTB).wrapping_add(cost);
        rec.set_u8(r::CTB, v);
        log::trace!("battle: MsCheckStatusAfterAction and 0078e5a0 for slot {slot:#x} are not run");
    }
}
