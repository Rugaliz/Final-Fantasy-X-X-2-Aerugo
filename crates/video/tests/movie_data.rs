//! Movies of a real installation (runs only when `AERUGO_GAME_DIR` is set and libvpx is installed).

use aerugo_vfs::{Vfs, install_from_env};
use aerugo_video::{Movie, MovieError, VpxError};

#[test]
fn movie_decodes_and_matches_ffmpeg() {
    let Some(inst) = install_from_env() else {
        eprintln!("skipped: no game installation (AERUGO_GAME_DIR)");
        return;
    };
    let mut vfs = Vfs::new();
    vfs.mount_install(&inst).unwrap();
    let path = "ffx_data/gamedata/ps3data/video/iwk_us.webm";
    let data = vfs.read(path).unwrap();
    let mut movie = match Movie::open(data.clone()) {
        Ok(m) => m,
        Err(MovieError::Vpx(VpxError::Missing(e))) => {
            eprintln!("skipped: libvpx not installed ({e})");
            return;
        }
        Err(e) => panic!("{e}"),
    };
    assert_eq!((movie.width, movie.height), (1280, 720));
    assert!(movie.has_audio());
    let (mut pictures, mut first, mut audio) = (0, None, 0usize);
    let mut t = 0i64;
    while !movie.ended() {
        movie.fill(t).unwrap();
        if let Some(p) = movie.picture_at(t) {
            first.get_or_insert(p);
            pictures += 1;
        }
        audio += movie.take_audio().len();
        t += 33_366_667;
    }
    let frames = audio / movie.audio_channels as usize;
    let seconds = frames as f64 / f64::from(movie.audio_rate);
    eprintln!("{pictures} pictures, {seconds:.2} s of audio, duration {:.2} s", movie.duration_s);
    assert!(pictures > 300);
    assert!((seconds - movie.duration_s).abs() < 0.2, "audio {seconds} s vs {} s", movie.duration_s);

    // The first picture is bit-exact with FFmpeg's VP8 decoder (when ffmpeg is installed).
    let first = first.unwrap();
    let input = std::env::temp_dir().join(format!("aerugo-movie-{}.webm", std::process::id()));
    std::fs::write(&input, &data).unwrap();
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&input)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output();
    let _ = std::fs::remove_file(&input);
    match out {
        Ok(o) if o.status.success() => {
            let y = first.y.len();
            let c = first.u.len();
            assert_eq!(o.stdout.len(), y + 2 * c);
            assert_eq!(&o.stdout[..y], &first.y[..], "Y plane");
            assert_eq!(&o.stdout[y..y + c], &first.u[..], "U plane");
            assert_eq!(&o.stdout[y + c..], &first.v[..], "V plane");
        }
        _ => eprintln!("ffmpeg comparison skipped"),
    }
}
