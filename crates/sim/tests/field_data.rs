//! Field events of a real installation load with their map and text, initialize, and run (needs `AERUGO_GAME_DIR`).

use aerugo_formats::Game;
use aerugo_formats::script;
use aerugo_sim::field::{Event, Input, model_path};
use aerugo_vfs::{ArchiveId, GAME_DIR_ENV, Language, Vfs, install_from_env};

#[test]
fn ffx_events_load_and_run() {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
        return;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let paths: Vec<String> = vfs
        .archives()
        .filter(|(id, _)| *id == Some(ArchiveId::Ffx))
        .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
        .filter(|p| p.ends_with(".ebp") && script::is_script_path(p))
        .collect();
    let (mut events, mut with_map, mut initialized, mut with_text, mut bound, mut models_found, mut opened) =
        (0, 0, 0, 0, 0, 0, 0);
    let mut missing_models = std::collections::BTreeSet::new();
    for path in &paths {
        let mut ev =
            Event::load(&vfs, Game::Ffx, path, Language::English).unwrap_or_else(|e| panic!("{path}: {e}"));
        events += 1;
        with_map += usize::from(ev.map_model.is_some());
        with_text += usize::from(!ev.state.messages.is_empty());
        let mut window_opened = false;
        for _ in 0..300 {
            ev.step(Input { confirm: true, ..Input::default() });
            window_opened |= ev.state.windows.iter().any(|w| w.to.open);
        }
        initialized += usize::from(ev.initialized());
        opened += usize::from(window_opened);
        for a in &ev.state.actors {
            if let Some(id) = a.model {
                bound += 1;
                match model_path(Game::Ffx, id) {
                    Some(p) if vfs.size(&p).is_some() => models_found += 1,
                    _ => {
                        missing_models.insert(id);
                    }
                }
            }
        }
    }
    eprintln!(
        "{events} events: {with_map} with a map, {with_text} with English text, {initialized} initialized in 10 s, \
         {opened} opened a message window; {bound} actors bound a model, {models_found} of those models found \
         (missing ids: {:x?})",
        missing_models.iter().take(20).collect::<Vec<_>>()
    );
    assert!(events > 300);
    assert!(initialized * 10 >= events * 9, "{initialized} of {events} initialized");
    assert!(with_text * 2 >= events, "{with_text} of {events} have text");
    assert!(models_found * 10 >= bound * 9, "{models_found} of {bound} models found");
}

#[test]
fn save_round_trips_through_an_event() {
    use aerugo_sim::save::{FFX_IMAGE_SIZE, SaveGame};
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
        return;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let path = "ffx_ps2/ffx/master/jppc/event/obj/bs/bsyt0000/bsyt0000.ebp";
    let mut ev = Event::load(&vfs, Game::Ffx, path, Language::English).unwrap();
    for _ in 0..90 {
        ev.step(Input::default());
    }
    let save = ev.save();
    assert_eq!(save.image.len(), FFX_IMAGE_SIZE);
    let back = SaveGame::from_bytes(&save.to_bytes()).unwrap();
    assert_eq!(back, save);
    let mut fresh = Event::load(&vfs, Game::Ffx, path, Language::English).unwrap();
    fresh.restore(&back);
    assert_eq!(&fresh.vm.memory.global[0][..FFX_IMAGE_SIZE], &save.image[..]);
    assert_eq!(fresh.tick, save.ticks);
}

/// With `AERUGO_RECORD_OUT` set, writes Aerugo's own run of an event as a recording (for checking the comparison
/// tool: comparing it must report no difference).
#[test]
fn record_own_run() {
    use aerugo_script::recording::{Record, encode};
    let (Some(inst), Some(out)) = (install_from_env(), std::env::var_os("AERUGO_RECORD_OUT")) else {
        eprintln!("skipped: needs {GAME_DIR_ENV} and AERUGO_RECORD_OUT");
        return;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let path = "ffx_ps2/ffx/master/jppc/event/obj/bs/bsyt0000/bsyt0000.ebp";
    let mut ev = Event::load(&vfs, Game::Ffx, path, Language::English).unwrap();
    ev.vm.trace = Some(Vec::new());
    let mut recs = vec![Record::Event {
        index: 1,
        path: "host0:/ffx/master/jppc/event/obj/bs/bsyt0000/bsyt0000.ebp".to_owned(),
    }];
    for f in 0..120u32 {
        recs.push(Record::Frame(f + 1));
        ev.step(Input::default());
        for t in ev.vm.trace.as_mut().unwrap().drain(..) {
            recs.push(Record::Instruction {
                worker: t.worker,
                level: t.level,
                pc: t.pc,
                op: t.op,
                operand: t.operand,
                depth: None,
            });
        }
    }
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("trace.bin"), encode(&recs)).unwrap();
}
