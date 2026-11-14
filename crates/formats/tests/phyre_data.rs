//! Phyre clusters of a real installation (runs only when `AERUGO_GAME_DIR` is set). Parses every 5th cluster;
//! `cargo run -p xtask -- check-phyre` parses all of them.

use aerugo_formats::phyre::{Cluster, PhyreError};
use aerugo_formats::texture::read_textures;
use aerugo_vfs::{GAME_DIR_ENV, Vfs, install_from_env};

#[test]
fn clusters_parse_and_textures_fill_their_data() {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
        return;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap_or_else(|e| panic!("{e}"));
    let (mut clusters, mut textures, mut ps3) = (0, 0, 0);
    for (_, archive) in vfs.archives() {
        for e in archive.entries().iter().filter(|e| e.path.ends_with(".phyre")).step_by(5) {
            let data = archive.read(e).unwrap_or_else(|err| panic!("{err}"));
            let c = match Cluster::parse(data) {
                Ok(c) => c,
                // A few PS3 clusters were shipped by mistake; the PC game cannot load them either.
                Err(PhyreError::Platform(p)) if &p == b"GCM\0" => {
                    ps3 += 1;
                    continue;
                }
                Err(err) => panic!("{}: {err}", e.path),
            };
            clusters += 1;
            let mut used = 0;
            for t in read_textures(&c) {
                let t = t.unwrap_or_else(|err| panic!("{}: {err}", e.path));
                used = t.levels.last().map_or(used, |r| r.end);
                textures += 1;
            }
            assert_eq!(used, c.texture_data.len(), "{}: texture levels must fill the data", e.path);
        }
    }
    eprintln!("{clusters} clusters, {textures} textures, {ps3} PS3 clusters skipped");
    assert!(clusters > 20_000 && textures > 20_000);
}
