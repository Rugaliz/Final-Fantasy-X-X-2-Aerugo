//! Tests against a real installation. They run only when `AERUGO_GAME_DIR` is set.

use aerugo_vfs::{ArchiveId, ArchiveStatus, GAME_DIR_ENV, Language, Vfs, install_from_env};

fn install() -> Option<aerugo_vfs::Install> {
    let inst = install_from_env();
    if inst.is_none() {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
    }
    inst
}

#[test]
fn installation_has_all_archives() {
    let Some(inst) = install() else { return };
    for id in ArchiveId::ALL {
        assert!(
            matches!(inst.archive_status(id), ArchiveStatus::Ok { .. }),
            "{:?}: {:?}",
            id,
            inst.archive_status(id)
        );
    }
}

/// Reads a spread of entries from every archive (the full check is `cargo xtask verify-archives`).
#[test]
fn sampled_entries_decompress_to_their_declared_size() {
    let Some(inst) = install() else { return };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap_or_else(|e| panic!("{e}"));
    let mut checked = 0;
    for (id, archive) in vfs.archives() {
        let entries = archive.entries();
        assert!(!entries.is_empty(), "{id:?} has entries");
        let step = (entries.len() / 200).max(1);
        for e in entries.iter().step_by(step).chain(entries.last()) {
            if e.size > 64 << 20 {
                continue; // skip very large videos here
            }
            let data = archive.read(e).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(data.len() as u64, e.size, "{}", e.path);
            checked += 1;
        }
    }
    assert!(checked > 300);
}

/// Every English overlay file in the FFX master tree is found when asking for its Japanese base path.
#[test]
fn language_overlay_resolves_on_real_data() {
    let Some(inst) = install() else { return };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap_or_else(|e| panic!("{e}"));
    let overlay = "/master/new_uspc/";
    let mut checked = 0;
    for (_, archive) in vfs.archives() {
        for e in archive.entries().iter().filter(|e| e.path.contains(overlay)).step_by(25) {
            let base = e.path.replace(overlay, "/master/jppc/");
            let (found, _) = vfs.locate_localized(&base, Language::English).expect("overlay found");
            assert_eq!(found, e.path);
            checked += 1;
        }
    }
    assert!(checked > 50, "only {checked} overlay files checked");
}
