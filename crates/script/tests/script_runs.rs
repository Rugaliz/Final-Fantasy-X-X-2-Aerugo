//! Every event and battle script of a real installation runs its init phase and a few seconds of frames without
//! machine faults (runs only when `AERUGO_GAME_DIR` is set). Natives without game state answer neutrally.

use std::collections::BTreeMap;
use std::sync::Arc;

use aerugo_formats::Game;
use aerugo_formats::script::{self, Script};
use aerugo_script::{Fault, Natives, Phase, Vm};
use aerugo_vfs::{ArchiveId, GAME_DIR_ENV, Vfs, install_from_env};

const FRAMES: u64 = 120;

#[test]
fn every_script_runs() {
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
        let mut natives = Natives::<()>::new(game);
        let (mut scripts, mut started, mut instructions) = (0, 0, 0usize);
        let mut faults: BTreeMap<String, usize> = BTreeMap::new();
        let mut examples: BTreeMap<String, String> = BTreeMap::new();
        for path in &paths {
            let data = vfs.read(path).unwrap();
            let Ok(s) = Script::parse(&data) else { continue };
            scripts += 1;
            let mut vm = Vm::new(Arc::new(s));
            vm.trace = Some(Vec::new());
            for _ in 0..FRAMES {
                vm.frame(&mut natives.host(&mut ()));
                instructions += vm.trace.as_mut().map_or(0, |t| std::mem::take(t).len());
            }
            if vm.phase == Phase::Running {
                started += 1;
            }
            for (_, _, f) in &vm.faults {
                let key = match f {
                    Fault::InvalidOpcode(_) => "invalid opcode".to_owned(),
                    other => format!("{other:?}"),
                };
                *faults.entry(key.clone()).or_default() += 1;
                examples.entry(key).or_insert_with(|| path.clone());
            }
        }
        eprintln!(
            "{}: {scripts} scripts, {started} finished their init phase within {FRAMES} frames, {instructions} \
             instructions; faults {faults:?}\n{}",
            game.name(),
            natives.coverage.report(game)
        );
        for (k, p) in &examples {
            eprintln!("  first {k}: {p}");
        }
        assert!(!faults.contains_key("invalid opcode") && !faults.contains_key("PcOutOfRange"), "{faults:?}");
        assert!(started * 10 >= scripts * 9, "{started} of {scripts} scripts started");
    }
}
