//! Sound banks of a real installation (runs only when `AERUGO_GAME_DIR` is set).

use aerugo_formats::fsb::{Bank, Codec, wav_xbox_ima};
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

/// Every 4th bank parses and decodes completely with the declared lengths (all of them take a long time to
/// read; `cargo run -p xtask -- fsb-compare sound_pc/` checks more).
#[test]
fn banks_parse_and_decode() {
    let Some(vfs) = vfs() else { return };
    let (mut banks, mut samples, mut decoded) = (0, 0, 0);
    for (_, archive) in vfs.archives() {
        for e in archive.entries().iter().filter(|e| e.path.ends_with(".fsb")).step_by(4) {
            let data = archive.read(e).unwrap();
            let bank = Bank::parse(&data).unwrap_or_else(|err| panic!("{}: {err}", e.path));
            assert_eq!(bank.codec, Codec::ImaAdpcm, "{}", e.path);
            banks += 1;
            samples += bank.samples.len();
            {
                for s in &bank.samples {
                    let pcm = bank.decode(&data, s).unwrap();
                    assert_eq!(pcm.len(), s.frames as usize * s.channels as usize, "{} {}", e.path, s.name);
                    // Encoded data covers the declared length (36 bytes per 64 frames per channel).
                    assert!(
                        s.data.len() * 64 / 36 >= s.frames as usize * s.channels as usize,
                        "{} {}",
                        e.path,
                        s.name
                    );
                    decoded += 1;
                }
            }
        }
    }
    eprintln!("{banks} banks, {samples} samples, {decoded} decoded");
    assert!(banks > 500);
}

/// Our decoder matches FFmpeg's Xbox IMA decoder sample for sample (skipped when ffmpeg is not installed).
#[test]
fn decoder_matches_ffmpeg() {
    let Some(vfs) = vfs() else { return };
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipped: ffmpeg not installed");
        return;
    }
    let dir = std::env::temp_dir();
    let mut compared = 0;
    for (_, archive) in vfs.archives() {
        for e in archive.entries().iter().filter(|e| e.path.ends_with(".fsb")).step_by(97) {
            let data = archive.read(e).unwrap();
            let bank = Bank::parse(&data).unwrap();
            let Some(s) = bank.samples.first() else { continue };
            let ours = bank.decode(&data, s).unwrap();
            let input = dir.join(format!("aerugo-fsb-test-{}.wav", std::process::id()));
            std::fs::write(&input, wav_xbox_ima(s.rate, s.channels, &data[s.data.clone()])).unwrap();
            let out = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-c:a", "adpcm_ima_xbox", "-i"])
                .arg(&input)
                .args(["-f", "s16le", "-"])
                .output()
                .unwrap();
            let _ = std::fs::remove_file(&input);
            let reference: Vec<i16> =
                out.stdout.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            assert!(reference.len() >= ours.len(), "{}: ffmpeg produced {} values", e.path, reference.len());
            assert_eq!(&reference[..ours.len()], &ours[..], "{} {}", e.path, s.name);
            compared += 1;
        }
    }
    eprintln!("{compared} samples identical to ffmpeg");
    assert!(compared > 10);
}
