//! PS2 motion groups of a real installation (runs only when `AERUGO_GAME_DIR` is set).

use aerugo_formats::motion::{MotionGroup, fit_translation_scale, motion_dir};
use aerugo_formats::phyre::Cluster;
use aerugo_vfs::{GAME_DIR_ENV, Vfs, install_from_env};

fn vfs() -> Option<Vfs> {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: set {GAME_DIR_ENV} to a game installation to run this test");
        return None;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap_or_else(|e| panic!("{e}"));
    Some(vfs)
}

/// Every non-empty motion group parses (a handful of shipped files are damaged).
#[test]
fn motion_groups_parse() {
    let Some(vfs) = vfs() else { return };
    let (mut groups, mut motions, mut failed) = (0, 0, Vec::new());
    for (_, archive) in vfs.archives() {
        for e in archive.entries().iter().filter(|e| e.path.ends_with(".mgrp") && e.size > 16) {
            match MotionGroup::parse(&archive.read(e).unwrap()) {
                Ok(g) => {
                    groups += 1;
                    motions += g.motions.len();
                }
                Err(err) => failed.push(format!("{}: {err}", e.path)),
            }
        }
    }
    eprintln!("{groups} groups, {motions} motions, {} failed: {failed:#?}", failed.len());
    assert!(groups > 4000 && motions > 60_000);
    assert!(failed.len() <= 6, "{failed:#?}");
}

/// Motion translations line up with HD skeletons of characters, monsters and summons: after fitting one scale
/// per model (about 0.01, smaller for some summons) the joint offsets match, which confirms that HD joint
/// `SkeletonNode_<n>` is PS2 joint n. Skill-effect objects and weapons are left out: their first motion group
/// often animates another object.
#[test]
fn motions_fit_hd_skeletons() {
    let Some(vfs) = vfs() else { return };
    let (mut checked, mut fitting, mut worst, mut misfits) = (0, 0, 0.0f32, Vec::<String>::new());
    for (_, archive) in vfs.archives() {
        for e in archive
            .entries()
            .iter()
            .filter(|e| {
                ["/chr/pc/", "/chr/npc/", "/chr/mon/", "/chr/sum/"].iter().any(|k| e.path.contains(k))
                    && e.path.ends_with(".dae.phyre")
            })
            .step_by(4)
        {
            let Some(dir) = motion_dir(&e.path) else { continue };
            let Some(mgrp) = archive
                .entries()
                .iter()
                .chain(vfs.archives().flat_map(|(_, a)| a.entries()))
                .find(|m| m.path.starts_with(&dir) && m.path.ends_with(".mgrp") && m.size > 16)
            else {
                continue;
            };
            let Ok(group) = MotionGroup::parse(&vfs.read(&mgrp.path).unwrap()) else { continue };
            let Some(motion) = group.motions.first() else { continue };
            let c = Cluster::parse(archive.read(e).unwrap()).unwrap();
            let model = aerugo_formats::mesh::read_model(&c).unwrap();
            let Some(sk) = model
                .meshes
                .iter()
                .filter_map(|m| aerugo_formats::skeleton::read_skeleton(&c, m.mesh))
                .max_by_key(|s| s.len())
            else {
                continue;
            };
            let k = fit_translation_scale(&sk.names, &sk.default_pose, motion);
            let (mut err, mut size, mut n) = (0.0f32, 0.0f32, 0);
            for (i, name) in sk.names.iter().enumerate() {
                let Some(j) = aerugo_formats::motion::ps2_joint(name).filter(|&j| j < motion.joints as usize)
                else {
                    continue;
                };
                let l = &sk.default_pose[i];
                let r = motion.joint(j, 0).translation;
                err += (0..3).map(|a| (l[12 + a] - r[a] * k).powi(2)).sum::<f32>().sqrt();
                size += (0..3).map(|a| l[12 + a].powi(2)).sum::<f32>().sqrt();
                n += 1;
            }
            if n == 0 || size == 0.0 {
                continue;
            }
            // Root and prop joints may move in the first frame; the skeleton as a whole must match.
            if err / size < 0.4 {
                fitting += 1;
            } else {
                misfits.push(format!("{}: {:.0}% (scale {k})", e.path, 100.0 * err / size));
            }
            worst = worst.max(err / size);
            checked += 1;
        }
    }
    eprintln!(
        "{checked} characters checked, {fitting} fit; worst relative offset error {worst:.3}; misfits {misfits:#?}"
    );
    assert!(checked > 20);
    assert!(fitting * 100 >= checked * 70, "{misfits:#?}");
}
