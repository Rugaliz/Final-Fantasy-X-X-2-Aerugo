//! Encounter areas of a real installation (needs `AERUGO_GAME_DIR`).

use aerugo_sim::encounter::EncounterTable;
use aerugo_vfs::{GAME_DIR_ENV, Vfs, install_from_env};

#[test]
fn encounter_areas_and_battle_labels() {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
        return;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let t =
        EncounterTable::parse(&vfs.read("ffx_ps2/ffx/master/jppc/battle/kernel/btl.bin").unwrap()).unwrap();
    assert!(t.areas.len() > 50);
    // Baaj temple: scripted battles only (danger 0), formations 0..2 as the scripts' labels name them.
    let baaj = t.area(0x20).unwrap();
    assert_eq!(t.areas[baaj].name, "bjyt02");
    assert_eq!(t.areas[baaj].groups[0].danger, 0);
    let b = t.label(0x0020_0001).unwrap();
    assert_eq!((b.area, b.group, b.formation, b.formation_id), (baaj, 0, 1, 1));
    // The Al Bhed ship's deck has random encounters.
    let ship = t.area(0x32).unwrap();
    assert_eq!(t.areas[ship].name, "cdsp00");
    assert_eq!(t.areas[ship].groups[0].danger, 60);
    assert!(t.areas[ship].groups[0].total_weight() > 0);
}
