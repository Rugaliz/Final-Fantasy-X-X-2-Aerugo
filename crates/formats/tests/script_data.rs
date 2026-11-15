//! Scripts of a real installation (runs only when `AERUGO_GAME_DIR` is set).

use std::collections::HashMap;

use aerugo_formats::Game;
use aerugo_formats::script::{self, Operand, Script};
use aerugo_vfs::{ArchiveId, GAME_DIR_ENV, Vfs, install_from_env};

#[test]
fn every_script_disassembles_with_our_native_names() {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
        return;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    for (archive, game) in [(ArchiveId::Ffx, Game::Ffx), (ArchiveId::Ffx2, Game::Ffx2)] {
        let paths: Vec<String> = vfs
            .archives()
            .filter(|(id, _)| *id == Some(archive))
            .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
            .filter(|p| script::is_script_path(p))
            .collect();
        let (mut scripts, mut workers, mut insns, mut empty) = (0usize, 0usize, 0usize, 0usize);
        let mut calls: HashMap<u16, usize> = HashMap::new();
        for path in &paths {
            let data = vfs.read(path).unwrap();
            if data.is_empty() {
                // A few unused FFX-2 map scripts are empty files.
                empty += 1;
                continue;
            }
            let s = Script::parse(&data).unwrap_or_else(|e| panic!("{path}: {e}"));
            scripts += 1;
            for (wi, w) in s.workers.iter().enumerate() {
                workers += 1;
                for i in s.walk(wi) {
                    insns += 1;
                    let info = i.info();
                    assert!(info.valid, "{path} worker {wi} at {:#x}: invalid opcode {:#x}", i.pc, i.op);
                    if info.branches {
                        let l = usize::from(i.operand.unwrap());
                        assert!(l < w.labels.len(), "{path} worker {wi} at {:#x}: label {l}", i.pc);
                    }
                    if info.operand == Operand::Native
                        && let Some(id) = i.operand
                    {
                        *calls.entry(id).or_default() += 1;
                    }
                }
            }
            if scripts <= 3 {
                assert!(!s.disassemble(0, game).is_empty());
            }
        }
        let total: usize = calls.values().sum();
        let named: usize =
            calls.iter().filter(|(id, _)| script::native(game, **id).info.is_some()).map(|(_, n)| n).sum();
        let pct = 100.0 * named as f64 / total.max(1) as f64;
        eprintln!(
            "{}: {scripts} scripts ({empty} empty files), {workers} workers, {insns} instructions, {} natives used, {total} calls, \
             {pct:.1}% to named natives",
            game.name(),
            calls.len()
        );
        assert!(scripts > 300, "{}: only {scripts} scripts", game.name());
        assert!(pct > 50.0, "{}: only {pct:.1}% of calls name a native", game.name());
    }
}
