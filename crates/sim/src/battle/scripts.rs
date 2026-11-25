//! The battle's script machines: the battle group (control 2: system_01's script and the battle file's) and the
//! monster group (control 3: one script per formation monster with a file, in slot order, copies included),
//! each a machine over several scripts whose workers have their own work areas (AtelBattleSetUp).
//!
//! Requests name a script space and an entry; a script's chunk 1 maps them to a worker and the entry it runs
//! (AtelGetActorExecspaceID), the worker counted from the script's first actor in the group (system_01 at 0, the
//! battle's after system_01's actors; a monster's after the actors of the records before its slot). Requests
//! are made at level 7.
//!
//! The machines run on their own native registry over the battle: natives not ported yet report a GAP.

use std::sync::Arc;

use aerugo_formats::Game;
use aerugo_formats::script::Script;
use aerugo_script::natives::{Natives, World};
use aerugo_script::vm::Vm;

use super::records::{self as r, Fields};
use super::{Battle, ChunkFile, battle_chunk, monster_chunk};

/// The level battle requests run at (MsAtelRequestExe).
const REQUEST_LEVEL: u8 = 7;

/// The machine (both controls) and its registry.
#[derive(Clone)]
pub struct BattleScripts {
    /// system_01 (battle file 1), read with the battle.
    pub system: ChunkFile,
    /// The battle control's workers first, then the monster control's.
    pub machine: Option<Vm>,
    pub natives: Natives<Battle>,
}

impl std::fmt::Debug for BattleScripts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BattleScripts").finish_non_exhaustive()
    }
}

impl World for Battle {}

/// The battle natives.
pub fn natives() -> Natives<Battle> {
    let mut n = Natives::new(Game::Ffx);
    super::natives::register(&mut n);
    n
}

/// 0x7976a0 (MsAtelChrInit's caller at set-up): the combatant every worker stands for, from the exec-space
/// tables: the battle's spaces 0x29–0x37 (slots 0–0xe), system_01's 5–0x20, 0x4f–0x6a and 0x6d–0x88 (slots
/// from 0) and seven fixed ones, each monster's spaces 0x3d, 4 and 0x40 (its slot).
pub fn bind_slots(scripts: &BattleScripts, b: &mut Battle) {
    let Some(vm) = &scripts.machine else { return };
    let mut slots = vec![0xffu8; vm.workers.len()];
    let system = scripts.system.chunk(battle_chunk::HEAD);
    let own = b.file.chunk(battle_chunk::HEAD);
    let own_base = usize::from(system.u8_at(0));
    let mut bind = |table: &[u8], base: usize, space: usize, slot: u8| {
        if let Some((w, _)) = exec_space(table, space, 0)
            && let Some(x) = slots.get_mut(base + usize::from(w))
        {
            *x = slot;
        }
    };
    log::trace!("battle: MsAtelChrInit(0, 2, 1, 0x3e) and (0, 2, 0, 0) are not ported");
    for space in 0x29..0x38 {
        bind(own, own_base, space, (space - 0x29) as u8);
    }
    for (first, end) in [(5, 0x21), (0x4f, 0x6b), (0x6d, 0x89)] {
        for space in first..end {
            bind(system, 0, space, (space - first) as u8);
        }
    }
    for (space, slot) in
        [(0x21, 0x1c), (0x22, 0x1d), (0x23, 0x1e), (0x25, 0xe), (0x26, 0xf), (0x27, 0x10), (0x28, 0x11)]
    {
        bind(system, 0, space, slot);
    }
    if let Some(monsters) = vm.control_start(1) {
        for i in 0..r::MONSTERS {
            let slot = r::FIRST_MONSTER + i;
            let Some(file) = b.monster_file(slot) else { continue };
            let base = monsters
                + (0..slot).map(|s| usize::from(b.records.get(s).u8_at(r::MONSTER_HEAD))).sum::<usize>();
            let table = file.chunk(monster_chunk::HEAD).to_vec();
            for space in [0x3d, 4, 0x40] {
                bind(&table, base, space, slot as u8);
            }
        }
    }
    b.worker_slots = slots;
}

/// Where a request's entry may come from (797b70 targets): the battle control's script 0 (system_01) or 1
/// (the battle's), or a monster slot's script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    BattleScript(usize),
    Monster(usize),
}

impl BattleScripts {
    pub fn new(system: ChunkFile) -> BattleScripts {
        BattleScripts { system, machine: None, natives: natives() }
    }
}

fn script_of(file: &ChunkFile, chunk: usize) -> Result<Arc<Script>, String> {
    Script::parse(&chunk_container(file.chunk(chunk)))
        .map(Arc::new)
        .map_err(|e| format!("battle script: {e:?}"))
}

/// A chunk as a one-chunk container (the script parser takes a container whose first chunk is the script).
fn chunk_container(chunk: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(chunk.len() + 16);
    v.extend_from_slice(&1u32.to_le_bytes());
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&[0; 8]);
    v.extend_from_slice(chunk);
    v
}

/// The machine: the battle control (797a40(0, …) for system_01 and the battle, 798390(0)) and the monster
/// control (MsSetRamMonData's 797a40(1, "mNNN", …) for every monster slot with a file, in slot order, 798390(1)).
pub fn machine(scripts: &BattleScripts, b: &Battle) -> Result<Vm, String> {
    let battle =
        vec![script_of(&scripts.system, battle_chunk::SCRIPT)?, script_of(&b.file, battle_chunk::SCRIPT)?];
    let mut monsters = Vec::new();
    for i in 0..r::MONSTERS {
        if let Some(file) = b.monster_file(r::FIRST_MONSTER + i) {
            monsters.push(script_of(file, monster_chunk::SCRIPT)?);
        }
    }
    Ok(Vm::controls(vec![battle, monsters], true))
}

/// AtelGetActorExecspaceID on a script's chunk 1: for `space` and `entry`, the worker (counted in the script)
/// and the entry it runs (`None` when the space has the worker but not the entry).
pub fn exec_space(table: &[u8], space: usize, entry: usize) -> Option<(u8, Option<u16>)> {
    let spaces = usize::from(table.u8_at(1));
    let index = table.u8_at(2 + space);
    if index == 0xff || space >= spaces {
        return None;
    }
    let record = (spaces.div_ceil(2) + usize::from(index) * 2) * 2;
    let list = table.u16_at(record + 4) as i16 as isize as usize;
    let worker = table.u8_at(record + 2);
    let entries = usize::from(table.u16_at(list));
    let run = (entry < entries).then(|| table.u16_at(list + 2 + entry * 2)).filter(|&e| e != 0xffff);
    Some((worker, run))
}

/// 00798590 for a target: its chunk-1 table, its script's place in the machine's script list (the battle
/// control's two, then one per monster slot with a file, by file order +0xdf5) and the machine worker its
/// workers count from.
fn target_table<'a>(
    scripts: &'a BattleScripts,
    b: &'a Battle,
    t: Target,
) -> Option<(&'a [u8], usize, usize)> {
    match t {
        Target::BattleScript(0) => Some((scripts.system.chunk(battle_chunk::HEAD), 0, 0)),
        Target::BattleScript(1) => {
            let base = usize::from(scripts.system.chunk(battle_chunk::HEAD).u8_at(0));
            Some((b.file.chunk(battle_chunk::HEAD), 1, base))
        }
        Target::BattleScript(_) => None,
        Target::Monster(slot) => {
            if slot >= r::FULL_SLOTS {
                return None;
            }
            let file = b.monster_file(slot)?;
            let control = scripts.machine.as_ref()?.control_start(1)?;
            let base = control
                + (0..slot).map(|s| usize::from(b.records.get(s).u8_at(r::MONSTER_HEAD))).sum::<usize>();
            Some((
                file.chunk(monster_chunk::HEAD),
                2 + usize::from(b.records.get(slot).u8_at(r::FILE_ORDER)),
                base,
            ))
        }
    }
}

/// MsAtelRequestExe: the worker `owner`'s table gives for `space` gets a request at level 7 running the entry
/// from the first of `targets` (script, space, entry) that has it. Returns the worker asked.
pub fn request_exe(
    scripts: &mut BattleScripts,
    b: &Battle,
    owner: Target,
    space: usize,
    targets: &[(Target, usize, usize)],
) -> Option<usize> {
    let (table, _, base) = target_table(scripts, b, owner)?;
    let (local, _) = exec_space(table, space, 0)?;
    let worker = base + usize::from(local);
    for &(t, t_space, entry) in targets {
        let Some((table, script, _)) = target_table(scripts, b, t) else { continue };
        let Some((descriptor, Some(run))) = exec_space(table, t_space, entry) else { continue };
        let vm = scripts.machine.as_mut()?;
        vm.request_in(worker, REQUEST_LEVEL, run, (script, usize::from(descriptor)));
        return Some(worker);
    }
    None
}

/// 797bc0: a battle event entry for `space`: from the battle's script, then the monster slot's (when given),
/// then system_01's, run by system_01's worker for the space.
pub fn request(
    scripts: &mut BattleScripts,
    b: &Battle,
    space: usize,
    monster: Option<usize>,
    entry: usize,
) -> Option<usize> {
    let mut targets = vec![(Target::BattleScript(1), space, entry)];
    if let Some(slot) = monster.filter(|&s| r::is_monster_slot(s)) {
        targets.push((Target::Monster(slot), space, entry));
    }
    targets.push((Target::BattleScript(0), space, entry));
    request_exe(scripts, b, Target::BattleScript(0), space, &targets)
}

/// Run a worker's new request at once (a request made with mode 1), with the save image swapped in.
pub fn run_now(scripts: &mut BattleScripts, battle: &mut Battle, globals: &mut [Vec<u8>; 2], worker: usize) {
    let Some(vm) = scripts.machine.as_mut() else { return };
    std::mem::swap(&mut vm.memory.global, globals);
    vm.run_now(worker, &mut scripts.natives.host(battle));
    std::mem::swap(&mut vm.memory.global, globals);
}

/// Run one frame of the machine with the field's global blocks (the save image) swapped in.
pub fn run_frame(scripts: &mut BattleScripts, battle: &mut Battle, globals: &mut [Vec<u8>; 2]) {
    let Some(vm) = scripts.machine.as_mut() else { return };
    std::mem::swap(&mut vm.memory.global, globals);
    vm.frame(&mut scripts.natives.host(battle));
    std::mem::swap(&mut vm.memory.global, globals);
}
