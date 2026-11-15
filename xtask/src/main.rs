//! Developer commands: `cargo run -p xtask -- <command>`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use aerugo_vfs::{ArchiveId, ArchiveStatus, GameId, Install, VbfArchive};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Aerugo developer commands")]
struct Cli {
    /// Game installation folder (default: $AERUGO_GAME_DIR, then Steam detection).
    #[arg(long, global = true)]
    game_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate an installation and show which programs it supports.
    CheckGame,
    /// Decompress every file of every archive and check its size (takes a few minutes).
    VerifyArchives {
        /// Worker threads (default: available cores).
        #[arg(long)]
        threads: Option<usize>,
    },
    /// List archive entries whose path contains a filter string.
    List { filter: Option<String> },
    /// Parse every Phyre cluster of both games (or every Nth with --every).
    CheckPhyre {
        #[arg(long, default_value_t = 1)]
        every: usize,
        /// Also list instance-list counts per class.
        #[arg(long)]
        verbose: bool,
        #[arg(long)]
        threads: Option<usize>,
    },
    /// Fit how PS2 motion channels map onto an HD skeleton (rotation order, translation scale).
    MotionFit {
        /// HD model archive path.
        model: String,
        /// Motion group archive path.
        mgrp: String,
    },
    /// Parse every motion group of both games.
    CheckMotions,
    /// Decode FSB5 samples and compare them with ffmpeg's Xbox IMA decoder, sample for sample.
    FsbCompare {
        /// Archive path filter for banks (e.g. "sound_pc/voice/us").
        filter: String,
        /// Banks to check (every Nth bank matching the filter).
        #[arg(long, default_value_t = 1)]
        every: usize,
        /// Samples per bank to check.
        #[arg(long, default_value_t = 4)]
        samples: usize,
    },
    /// Write one decoded FSB5 sample as a WAV file.
    FsbExport { bank: String, index: usize, out: PathBuf },
    /// Count vertex stream layouts, primitive and index types over all model clusters.
    ModelCensus,
    /// Compare a recording of the original game (the oracle's output folder) with Aerugo, event by event.
    OracleCompare {
        /// Folder with `trace.bin` and `saves/`.
        dir: PathBuf,
        /// Only the event run with this index in the recording (0 = first).
        #[arg(long)]
        run: Option<usize>,
    },
    /// Measure native argument counts on a version-2 recording and list those that disagree with the table.
    OracleArity { dir: PathBuf },
    /// List every turn in a recording: its speed arguments, mode, start and target yaw, and how long it took.
    OracleTurns { dir: PathBuf },
    /// List every message window a recording opens: the setup natives' arguments and how many frames until the
    /// window reports its text done.
    OracleWindows { dir: PathBuf },
    /// Run an event headless with no recording to lean on: confirm every `--confirm-every` ticks and push the
    /// stick forward, printing the player, control and windows now and then, and the natives Aerugo lacks.
    FieldRun {
        event: String,
        #[arg(long, default_value_t = 3000)]
        ticks: u32,
        /// Save image to start from (a recording's `saves/*.img`).
        #[arg(long)]
        image: Option<PathBuf>,
        #[arg(long, default_value_t = 60)]
        confirm_every: u32,
        /// Push the stick from this tick on.
        #[arg(long, default_value_t = u32::MAX)]
        walk_from: u32,
        /// Stick direction while walking: x (right) and y (forward), −1..1.
        #[arg(long, num_args = 2, default_values_t = [0.0, 1.0], allow_negative_numbers = true)]
        stick: Vec<f32>,
        /// Instead of a fixed stick, steer through these ground points ("x,z;x,z;…") and confirm at the last.
        #[arg(long, allow_hyphen_values = true)]
        goto: Option<String>,
        /// How far from the point to let go of the stick (the walk slows down over a few more ticks).
        #[arg(long, default_value_t = 4.0)]
        stop: f32,
    },
    /// Run every FFX field event in the field engine for a while (confirming now and then) and print which
    /// natives the scripts call and which Aerugo lacks, by call count.
    FieldCoverage {
        #[arg(long, default_value_t = 300)]
        frames: u32,
    },
    /// List the motions scripts play with a frame range (native 0x501A) and how many ticks the next motion
    /// wait polls as not done.
    OracleMotions { dir: PathBuf },
    /// List each call of the given natives (hex ids) in a recording: event, frame, worker, the pushed values
    /// before the call, the result and how many polls it waited.
    OracleNatives {
        dir: PathBuf,
        #[arg(value_parser = parse_hex_u16)]
        natives: Vec<u16>,
    },
    /// Count the calls of natives the field runner lacks in a recording, per event and in all.
    OracleMissing { dir: PathBuf },
    /// Print the text fields of kernel table records (English): `kernel-text btlend_txt 0x16 0x1d`.
    KernelText {
        table: String,
        #[arg(value_parser = parse_hex_u16)]
        first: u16,
        #[arg(value_parser = parse_hex_u16)]
        last: u16,
    },
    /// Every call of a native in FFX event scripts with the constants pushed right before it, counted.
    NativeArgs {
        #[arg(value_parser = parse_hex_u16)]
        native: u16,
    },
    /// The workers (worker:level, in order) that ran in each frame of a range of one event run.
    OracleFrames { dir: PathBuf, run: usize, from: u32, to: u32 },
    /// Count random numbers per event and stream in a recording (first values shown).
    OracleRandom { dir: PathBuf },
    /// Print a movie's length and its first video frame times (number from the video list, or a path).
    MovieInfo { movie: String },
    /// Print the start of an archive file as hex.
    Hexdump {
        path: String,
        #[arg(long, default_value_t = 256)]
        len: usize,
    },
    /// Disassemble one worker of an event script (event as the recordings name it, e.g. `zn/znkd0900`).
    ScriptDump { event: String, worker: usize },
    /// Print the frames of a recording where the scripts' pressed pad word is not zero.
    OracleInput {
        dir: PathBuf,
        #[arg(long)]
        run: Option<usize>,
    },
    /// Print one worker's recorded position and yaw, polls and results frame by frame in one event run.
    OracleTrack {
        dir: PathBuf,
        #[arg(long)]
        run: usize,
        #[arg(long)]
        worker: u16,
        #[arg(long, default_value_t = 40)]
        frames: u32,
    },
    /// Run every script for a few seconds with the implemented natives and print native coverage.
    NativeCoverage {
        #[arg(long, default_value_t = 120)]
        frames: u64,
    },
    /// Rank effects by how many model segments use them, with their main pass state.
    EffectCensus {
        /// How many effects to list.
        #[arg(long, default_value_t = 30)]
        top: usize,
    },
    /// Print a Phyre cluster: header, instance lists and every object's fields.
    DumpPhyre {
        /// Archive path (as printed by `list`).
        path: String,
        /// Only objects of this class (or derived ones).
        #[arg(long)]
        class: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let install = find_install(cli.game_dir)?;
    match cli.command {
        Command::CheckGame => check_game(&install),
        Command::VerifyArchives { threads } => verify(&install, threads),
        Command::List { filter } => list(&install, filter.as_deref()),
        Command::CheckPhyre { every, verbose, threads } => {
            check_phyre(&install, every.max(1), verbose, threads)
        }
        Command::ModelCensus => model_census(&install),
        Command::EffectCensus { top } => effect_census(&install, top),
        Command::NativeCoverage { frames } => native_coverage(&install, frames),
        Command::OracleCompare { dir, run } => oracle_compare(&install, &dir, run),
        Command::OracleArity { dir } => oracle_arity(&dir),
        Command::OracleTurns { dir } => oracle_turns(&install, &dir),
        Command::OracleInput { dir, run } => oracle_input(&dir, run),
        Command::OracleWindows { dir } => oracle_windows(&install, &dir),
        Command::ScriptDump { event, worker } => script_dump(&install, &event, worker),
        Command::FieldRun { event, ticks, image, confirm_every, walk_from, stick, goto, stop } => {
            let run = FieldRunPlan {
                ticks,
                image,
                confirm_every,
                walk_from,
                stick: [stick[0], stick[1]],
                goto: goto
                    .unwrap_or_default()
                    .split(';')
                    .filter_map(|p| {
                        let v: Vec<f32> = p.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                        (v.len() == 2).then(|| [v[0], v[1]])
                    })
                    .collect(),
                stop,
            };
            field_run(&install, &event, &run)
        }
        Command::FieldCoverage { frames } => field_coverage(&install, frames),
        Command::OracleMotions { dir } => oracle_motions(&dir),
        Command::MovieInfo { movie } => movie_info(&install, &movie),
        Command::NativeArgs { native } => native_args(&install, native),
        Command::KernelText { table, first, last } => kernel_text(&install, &table, first, last),
        Command::OracleMissing { dir } => oracle_missing(&dir),
        Command::OracleRandom { dir } => oracle_random(&dir),
        Command::OracleFrames { dir, run, from, to } => oracle_frames(&dir, run, from, to),
        Command::OracleNatives { dir, natives } => oracle_natives(&dir, &natives),
        Command::Hexdump { path, len } => {
            let mut vfs = aerugo_vfs::Vfs::new();
            vfs.mount_install(&install)?;
            let data = vfs.read(&path)?;
            for (i, row) in data[..len.min(data.len())].chunks(16).enumerate() {
                let hex: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
                let text: String =
                    row.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
                println!("{:06x}  {:<48} {text}", i * 16, hex.join(" "));
            }
            Ok(())
        }
        Command::OracleTrack { dir, run, worker, frames } => oracle_track(&dir, run, worker, frames),
        Command::MotionFit { model, mgrp } => motion_fit(&install, &model, &mgrp),
        Command::CheckMotions => check_motions(&install),
        Command::FsbCompare { filter, every, samples } => {
            fsb_compare(&install, &filter, every.max(1), samples)
        }
        Command::FsbExport { bank, index, out } => fsb_export(&install, &bank, index, &out),
        Command::DumpPhyre { path, class } => dump_phyre(&install, &path, class.as_deref()),
    }
}

fn find_install(dir: Option<PathBuf>) -> Result<Install> {
    if let Some(d) = dir {
        return Ok(Install::inspect(d));
    }
    if let Some(i) = aerugo_vfs::install_from_env() {
        return Ok(i);
    }
    aerugo_vfs::install::discover().into_iter().next().context("no installation found; use --game-dir")
}

fn check_game(install: &Install) -> Result<()> {
    println!("Installation: {}", install.root.display());
    for (id, status) in &install.archives {
        println!("  {:<24} {:?}", id.relative_path(), status);
    }
    for g in GameId::ALL {
        let ok = install.has_data_for(g);
        println!("  {:<36} {}", g.title(), if ok { "data present" } else { "data missing" });
    }
    if !install.is_usable() {
        bail!("installation is not usable");
    }
    Ok(())
}

fn open_all(install: &Install) -> Result<Vec<VbfArchive>> {
    let mut v = Vec::new();
    for id in ArchiveId::ALL {
        if let ArchiveStatus::Ok { .. } = install.archive_status(id) {
            v.push(VbfArchive::open(install.archive_path(id))?);
        }
    }
    Ok(v)
}

fn list(install: &Install, filter: Option<&str>) -> Result<()> {
    let filter = filter.map(str::to_ascii_lowercase);
    for a in open_all(install)? {
        for e in a.entries() {
            if filter.as_ref().is_none_or(|f| e.path.to_ascii_lowercase().contains(f)) {
                println!("{:>12}  {}", e.size, e.path);
            }
        }
    }
    Ok(())
}

fn verify(install: &Install, threads: Option<usize>) -> Result<()> {
    let threads = threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));
    let mut failures = 0;
    for archive in open_all(install)? {
        let start = Instant::now();
        let next = AtomicUsize::new(0);
        let bytes = AtomicU64::new(0);
        let errors = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(e) = archive.entries().get(i) else { break };
                        match archive.read(e) {
                            Ok(data) if data.len() as u64 == e.size => {
                                bytes.fetch_add(e.size, Ordering::Relaxed);
                            }
                            Ok(data) => errors.lock().unwrap().push(format!(
                                "{}: {} bytes, expected {}",
                                e.path,
                                data.len(),
                                e.size
                            )),
                            Err(err) => errors.lock().unwrap().push(err.to_string()),
                        }
                    }
                });
            }
        });
        let errors = errors.into_inner().unwrap();
        println!(
            "{}: {} entries, {:.1} GiB in {:.0?}, {} errors",
            archive.path().display(),
            archive.entries().len(),
            bytes.load(Ordering::Relaxed) as f64 / (1u64 << 30) as f64,
            start.elapsed(),
            errors.len()
        );
        for e in errors.iter().take(20) {
            println!("  {e}");
        }
        failures += errors.len();
    }
    if failures > 0 {
        bail!("{failures} entries failed");
    }
    Ok(())
}

fn threads_or_default(threads: Option<usize>) -> usize {
    threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
}

fn check_phyre(install: &Install, every: usize, verbose: bool, threads: Option<usize>) -> Result<()> {
    use aerugo_formats::phyre::Cluster;
    let threads = threads_or_default(threads);
    let mut failures = Vec::new();
    for archive in open_all(install)? {
        let entries: Vec<_> =
            archive.entries().iter().filter(|e| e.path.ends_with(".phyre")).step_by(every).collect();
        let start = Instant::now();
        let next = AtomicUsize::new(0);
        let ok = AtomicUsize::new(0);
        let errors = std::sync::Mutex::new(Vec::new());
        let classes = std::sync::Mutex::new(std::collections::BTreeMap::<String, usize>::new());
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    let mut local = std::collections::BTreeMap::<String, usize>::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(e) = entries.get(i) else { break };
                        let result = match archive.read(e) {
                            Ok(d) => Cluster::parse(d),
                            Err(x) => {
                                errors.lock().unwrap().push(format!("{}: {x}", e.path));
                                continue;
                            }
                        };
                        match result {
                            Ok(c) => {
                                ok.fetch_add(1, Ordering::Relaxed);
                                for l in &c.lists {
                                    *local
                                        .entry(format!("class {}", c.namespace.classes[l.class].name))
                                        .or_default() += 1;
                                }
                                let mut used = 0;
                                for t in aerugo_formats::texture::read_textures(&c) {
                                    match t {
                                        Ok(t) => {
                                            *local
                                                .entry(format!("format {} faces {}", t.format_name, t.faces))
                                                .or_default() += 1;
                                            used = t.levels.last().map_or(used, |r| r.end);
                                        }
                                        Err(err) => {
                                            *local.entry("texture error".into()).or_default() += 1;
                                            errors.lock().unwrap().push(format!("{}: {err}", e.path));
                                        }
                                    }
                                }
                                if !c.texture_data.is_empty() {
                                    let k = if used == c.texture_data.len() {
                                        "texture data exact"
                                    } else {
                                        "texture data NOT exact"
                                    };
                                    *local.entry(k.into()).or_default() += 1;
                                    if used != c.texture_data.len() {
                                        errors.lock().unwrap().push(format!(
                                            "{}: texture data {} bytes, levels use {used}",
                                            e.path,
                                            c.texture_data.len()
                                        ));
                                    }
                                }
                            }
                            Err(aerugo_formats::PhyreError::Platform(p)) => {
                                *local
                                    .entry(format!("platform {}", String::from_utf8_lossy(&p)))
                                    .or_default() += 1;
                            }
                            Err(err) => errors.lock().unwrap().push(format!("{}: {err}", e.path)),
                        }
                    }
                    let mut all = classes.lock().unwrap();
                    for (k, v) in local {
                        *all.entry(k).or_default() += v;
                    }
                });
            }
        });
        let errors = errors.into_inner().unwrap();
        println!(
            "{}: {} of {} clusters parsed in {:.0?}, {} failed",
            archive.path().display(),
            ok.load(Ordering::Relaxed),
            entries.len(),
            start.elapsed(),
            errors.len()
        );
        for e in errors.iter().take(20) {
            println!("  {e}");
        }
        let classes = classes.into_inner().unwrap();
        for (k, v) in &classes {
            if !k.starts_with("class ") || verbose {
                println!("  {k}: {v}");
            }
        }
        failures.extend(errors);
    }
    if !failures.is_empty() {
        bail!("{} clusters failed", failures.len());
    }
    Ok(())
}

fn dump_phyre(install: &Install, path: &str, class: Option<&str>) -> Result<()> {
    use aerugo_formats::phyre::Cluster;
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let c = Cluster::parse(vfs.read(path)?)?;
    println!("{:#?}", c.header);
    println!(
        "buffers: index {} bytes, vertex {} bytes, texture {} bytes",
        c.index_buffer.len(),
        c.vertex_buffer.len(),
        c.texture_data.len()
    );
    for (i, l) in c.lists.iter().enumerate() {
        println!(
            "list {i:>3}: {:<40} {:>5} × {:>5} bytes, arrays {} bytes",
            c.namespace.classes[l.class].name,
            l.count,
            l.object_size,
            l.arrays.len()
        );
    }
    for (i, l) in c.lists.iter().enumerate() {
        if class.is_some_and(|k| !c.namespace.is_a(l.class, k)) {
            continue;
        }
        for index in 0..l.count {
            let id = aerugo_formats::ObjectId { list: i as u32, index };
            println!("\n{}[{index}]", c.namespace.classes[l.class].name);
            for f in c.describe(id) {
                println!("  {:#06x} {:<36} {:<28} {}", f.offset, f.name, f.type_name, f.value);
            }
        }
    }
    Ok(())
}

/// The archive path of an event as the original names it (`host0:/ffx/master/jppc/event/...`).
/// What a headless field run does (see `FieldRun`).
struct FieldRunPlan {
    ticks: u32,
    image: Option<PathBuf>,
    confirm_every: u32,
    walk_from: u32,
    stick: [f32; 2],
    /// Points to steer through (x, z), and how far from the last to let go.
    goto: Vec<[f32; 2]>,
    stop: f32,
}

fn field_run(install: &Install, event: &str, plan: &FieldRunPlan) -> Result<()> {
    let FieldRunPlan { ticks, ref image, confirm_every, walk_from, stick, ref goto, stop } = *plan;
    let mut leg = 0usize;
    use aerugo_formats::Game;
    use aerugo_sim::field::{Event, Input};
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let path = archive_event_path(event).unwrap_or_else(|| event.to_owned());
    let game = if path.contains("/ffx2/") { Game::Ffx2 } else { Game::Ffx };
    let mut ev = Event::load(&vfs, game, &path, aerugo_vfs::Language::English).map_err(anyhow::Error::msg)?;
    // Event::load reads the walk mesh; without one, the drawn map's triangles.
    if ev.state.ground.is_some() {
        println!("ground from the walk mesh");
    } else if let Some(p) = &ev.map_model
        && let Ok(d) = vfs.read(p)
        && let Ok(c) = aerugo_formats::phyre::Cluster::parse(d)
        && let Ok(model) = aerugo_formats::mesh::read_model(&c)
    {
        ev.state.ground = Some(aerugo_sim::field::collision::Ground::from_model(&c, &model));
        println!("ground from {p}");
    }
    // FIELD_PROBE="x z;x z": floor heights there.
    if let (Ok(probe), Some(g)) = (std::env::var("FIELD_PROBE"), ev.state.ground.as_ref()) {
        for pt in probe.split(';') {
            let v: Vec<f32> = pt.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if let [x, z] = v[..] {
                let hs: Vec<f32> = g.heights(x, z).collect();
                println!("floor at {x} {z}: {hs:?}");
            }
        }
    }
    if let Some(img) = image {
        let img = std::fs::read(img)?;
        let n = img.len().min(ev.vm.memory.global[0].len());
        ev.vm.memory.global[0][..n].copy_from_slice(&img[..n]);
    }
    for t in 0..ticks {
        if ev.state.player.is_none() && ev.initialized() {
            ev.state.player = ev.state.actors.iter().position(|a| a.model.is_some_and(|m| m >> 12 == 0));
        }
        let mut input = Input {
            confirm: confirm_every > 0 && t % confirm_every == confirm_every - 1,
            stick: if t >= walk_from { stick } else { [0.0; 2] },
            ..Input::default()
        };
        // Steering (no camera: forward is +Z, right is +X): head for the point, then stand and confirm.
        if let (Some(&[gx, gz]), true) = (goto.get(leg), t >= walk_from)
            && let Some(p) = ev.state.player.and_then(|p| ev.state.actors.get(p)).map(|a| a.pos)
        {
            let (dx, dz) = (gx - p[0], gz - p[2]);
            let d = (dx * dx + dz * dz).sqrt();
            let last = leg + 1 == goto.len();
            if !last && d < 3.0 {
                leg += 1;
            }
            if !last || d > stop {
                input.stick = [dx / d, dz / d];
            } else {
                input.stick = [0.0; 2];
                input.confirm = t % 30 == 0;
            }
        }
        ev.load_motion_frames(&vfs);
        if let Some(path) = ev.movie_needing_length() {
            let ticks = movie_ticks(&vfs, path).unwrap_or(0);
            ev.set_movie_frames(ticks);
        }
        ev.step(input);
        if ev.open_menu().is_some() {
            ev.close_menu();
        }
        if let Some(b) = ev.running_battle() {
            println!("t{t}: battle {} — won (placeholder)", ev.battle_name(&b));
            ev.finish_battle();
        }
        if t % 100 == 0 || ev.state.jump.is_some() {
            let p = ev.state.player.and_then(|p| ev.state.actors.get(p)).map(|a| a.pos);
            let windows: Vec<String> = ev
                .state
                .windows
                .iter()
                .enumerate()
                .filter(|(_, w)| w.open)
                .map(|(i, w)| format!("{i}:{:?} {}/{}", w.message, w.shown, w.reveal))
                .collect();
            let talking: Vec<String> = ev
                .vm
                .workers
                .iter()
                .enumerate()
                .filter(|(_, w)| w.active && w.level == 7 && w.kind == 1)
                .map(|(i, w)| format!("w{i} l7 e{}", w.entry))
                .collect();
            println!(
                "t{t} player {:?} {p:?} control {} windows {windows:?} level7 {talking:?} map {:?}",
                ev.state.player, ev.state.user_control, ev.state.jump
            );
            if ev.state.jump.is_some() {
                break;
            }
        }
    }
    println!("{}", ev.natives.coverage.report(game));
    Ok(())
}

fn script_dump(install: &Install, event: &str, worker: usize) -> Result<()> {
    use aerugo_formats::Game;
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let path = archive_event_path(event).ok_or_else(|| anyhow::anyhow!("event path not understood"))?;
    let game = if path.contains("/ffx2/") { Game::Ffx2 } else { Game::Ffx };
    let script = aerugo_formats::script::Script::parse(&vfs.read(&path)?)?;
    for (i, e) in script.entrances().iter().enumerate() {
        println!(
            "; entrance {i}: entry {} heading {} at {:?} (worker {:?})",
            e.entry,
            e.yaw,
            e.pos,
            script.entrance_entry(i).map(|w| w.0)
        );
    }
    print!("{}", script.disassemble(worker, game));
    Ok(())
}

fn oracle_windows(install: &Install, dir: &std::path::Path) -> Result<()> {
    use aerugo_formats::Game;
    use aerugo_formats::script::op_info;
    use aerugo_script::recording::{Record, parse};
    use aerugo_sim::field::Event;
    use std::collections::HashMap;
    #[derive(Default)]
    struct W {
        pushes: Vec<u16>,
        setup: Vec<String>,
        // (frame opened, setup, polls not done)
        open: Option<(u32, String, u32)>,
        last_call: u16,
        message: Option<usize>,
    }
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let (mut frame, mut name) = (0u32, String::new());
    let mut messages: Vec<aerugo_sim::field::Message> = Vec::new();
    let mut ws: HashMap<u16, W> = HashMap::new();
    for r in &records {
        match r {
            Record::Event { path, .. } => {
                ws.clear();
                frame = 0;
                name = path.clone();
                messages = archive_event_path(path)
                    .and_then(|p| {
                        let game = if p.contains("/ffx2/") { Game::Ffx2 } else { Game::Ffx };
                        Event::load(&vfs, game, &p, aerugo_vfs::Language::English).ok()
                    })
                    .map(|e| e.state.messages.clone())
                    .unwrap_or_default();
            }
            Record::Frame(_) => frame += 1,
            Record::Instruction { worker, op, operand, .. } => {
                let w = ws.entry(*worker).or_default();
                let m = op_info(*op).mnemonic;
                if m == "push.imm" {
                    w.pushes.push(operand.unwrap_or(0));
                } else if m.starts_with("call") {
                    let id = operand.unwrap_or(0);
                    w.last_call = id;
                    let args: Vec<String> = w.pushes.iter().map(|v| format!("{v:#x}")).collect();
                    if id == 0x64 {
                        w.message = w.pushes.last().map(|&m| usize::from(m));
                    }
                    if matches!(id, 0x64 | 0x65 | 0x66 | 0x9d | 0x6a) {
                        w.setup.push(format!("{id:x}({})", args.join(",")));
                    }
                    if id == 0x6a {
                        w.open = Some((frame, w.setup.join(" "), 0));
                        w.setup.clear();
                    }
                    w.pushes.clear();
                } else {
                    w.pushes.clear();
                }
            }
            Record::NativeResult { worker, native: 0x8f, value, .. } => {
                let w = ws.entry(*worker).or_default();
                if let Some((f0, setup, n)) = &mut w.open {
                    if *value == 0 {
                        *n += 1;
                    } else {
                        let m = w.message.and_then(|m| messages.get(m));
                        println!(
                            "{name} w{worker} f{f0}: done after {} frames ({n} not done) {setup} length {:?} {:?}",
                            frame - *f0,
                            m.map(|m| m.len()),
                            m.map(|m| m.raw.as_str()).unwrap_or_default()
                        );
                        w.open = None;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn oracle_input(dir: &std::path::Path, only: Option<usize>) -> Result<()> {
    use aerugo_script::recording::{event_runs, parse};
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    for (i, run) in event_runs(&records).iter().enumerate() {
        if only.is_some_and(|o| o != i) {
            continue;
        }
        println!("run {i} {}", run.path);
        let word =
            |d: &[u8], at: usize| d.get(at..at + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
        // INPUT_PAD: every frame's start of pad 0's state block instead.
        if std::env::var("INPUT_PAD").is_ok() {
            for (f, d) in run.inputs.iter().enumerate() {
                let pad: Vec<String> = d.iter().skip(0x20).take(0x20).map(|b| format!("{b:02x}")).collect();
                println!("  f{f}: {}", pad.join(""));
            }
            continue;
        }
        for (f, d) in run.inputs.iter().enumerate() {
            let words: Vec<String> = (0..8).map(|k| format!("{:08x}", word(d, k * 4))).collect();
            if words.iter().skip(4).any(|w| w != "00000000") {
                println!("  f{f}: {}", words.join(" "));
            }
        }
    }
    Ok(())
}

fn oracle_turns(install: &Install, dir: &std::path::Path) -> Result<()> {
    use aerugo_formats::script::{Operand, Script, op_info};
    use aerugo_script::recording::{Record, parse};
    use std::collections::HashMap;
    #[derive(Default)]
    struct Turn {
        pushes: Vec<f64>,
        speed: (f64, f64),
        target: f32,
        // (frame started, mode, yaw at the start, not-done polls)
        active: Option<(u32, u32, f32, u32)>,
        yaw: f32,
        last_yaw: f32,
        camera_time: Vec<f64>,
        camera_wait: Option<(u32, u32)>,
    }
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let (mut script, mut frame, mut name) = (None::<Script>, 0u32, String::new());
    let mut turns: HashMap<u16, Turn> = HashMap::new();
    for r in &records {
        match r {
            Record::Event { path, .. } => {
                turns.clear();
                frame = 0;
                script = archive_event_path(path).and_then(|p| {
                    name = p.rsplit('/').next().unwrap_or_default().to_owned();
                    Script::parse(&vfs.read(&p).ok()?).ok()
                });
            }
            Record::Frame(_) => frame += 1,
            Record::Positions(ps) => {
                for p in ps {
                    let t = turns.entry(p.worker).or_default();
                    t.last_yaw = t.yaw;
                    t.yaw = p.yaw;
                }
            }
            Record::Instruction { worker, op, operand, .. } => {
                let t = turns.entry(*worker).or_default();
                let info = op_info(*op);
                match (info.mnemonic, operand) {
                    ("push.float", Some(v)) => {
                        let f = script.as_ref().and_then(|s| {
                            s.workers.get(usize::from(*worker))?.floats.get(usize::from(*v)).copied()
                        });
                        t.pushes.push(f64::from(f.unwrap_or(f32::NAN)));
                    }
                    (m, Some(v)) if m.starts_with("push.") && info.operand != Operand::Var => {
                        t.pushes.push(f64::from(*v as i16));
                    }
                    (m, Some(0x1d)) if m.starts_with("call") => {
                        let n = t.pushes.len();
                        if n >= 2 {
                            t.speed = (t.pushes[n - 2], t.pushes[n - 1]);
                        }
                    }
                    (m, Some(0x6014)) if m.starts_with("call") => {
                        t.camera_time = t.pushes[t.pushes.len().saturating_sub(4)..].to_vec();
                    }
                    (m, Some(0x19)) if m.starts_with("call") => {
                        let n = t.pushes.len();
                        let mode = if n >= 2 { t.pushes[n - 2] as i64 as u32 } else { u32::MAX };
                        t.active = Some((frame, mode, t.yaw, 0));
                    }
                    _ => {}
                }
                if t.pushes.len() > 8 {
                    t.pushes.remove(0);
                }
            }
            Record::NativeResult { worker, native: 0x28, tag: 2, value } => {
                turns.entry(*worker).or_default().target = f32::from_bits(*value);
            }
            Record::NativePoll { worker, native: 0x601a, flags } => {
                let t = turns.entry(*worker).or_default();
                let (start, polls) = t.camera_wait.get_or_insert((frame, 0));
                if *flags == 0 {
                    *polls += 1;
                } else {
                    println!(
                        "{name:<10} w{worker:<3} f{start:<6} camera time {:?} not done {polls}",
                        t.camera_time
                    );
                    t.camera_wait = None;
                }
            }
            Record::NativePoll { worker, native: 0x1b, flags } => {
                let t = turns.entry(*worker).or_default();
                if let Some((start, mode, yaw0, polls)) = &mut t.active {
                    if *flags == 0 {
                        *polls += 1;
                    } else {
                        println!(
                            "{name:<10} w{worker:<3} f{start:<6} speed ({}, {}) mode {mode:#06x} yaw {yaw0:.4} -> {:.4} \
                             ({:.4}) not done {polls}; last step {:.5}",
                            t.speed.0,
                            t.speed.1,
                            t.target,
                            t.yaw,
                            t.yaw - t.last_yaw
                        );
                        t.active = None;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn oracle_track(dir: &std::path::Path, run: usize, worker: u16, frames: u32) -> Result<()> {
    use aerugo_script::recording::{Record, parse};
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let (mut current, mut frame) = (None::<usize>, 0u32);
    let mut line = String::new();
    for r in &records {
        match r {
            Record::Event { .. } => {
                current = Some(current.map_or(0, |c| c + 1));
                frame = 0;
            }
            Record::Frame(_) => {
                if current == Some(run) && !line.is_empty() {
                    println!("f{frame:<5}{line}");
                }
                line.clear();
                frame += 1;
                if current == Some(run) && frame > frames {
                    break;
                }
            }
            _ if current != Some(run) => {}
            Record::Positions(ps) => {
                for p in ps.iter().filter(|p| p.worker == worker) {
                    line += &format!(
                        " pos ({:.2} {:.2} {:.2}) pitch {:.4} yaw {:.4}",
                        p.pos[0], p.pos[1], p.pos[2], p.pitch, p.yaw
                    );
                }
            }
            Record::Instruction { worker: w, op, operand, level, pc, .. }
                if *w == worker || worker == 0xfffe =>
            {
                if worker == 0xfffe {
                    line += &format!(" w{w}");
                }
                let m = aerugo_formats::script::op_info(*op).mnemonic;
                line += &format!(
                    " [l{level} {pc:x} {m}{}]",
                    operand.map(|v| format!(" {v:#x}")).unwrap_or_default()
                );
            }
            Record::NativePoll { worker: w, native, flags } if *w == worker => {
                line += &format!(" poll {native:#06x}={flags}");
            }
            // Worker 0xffff: camera natives of every worker.
            Record::NativePoll { worker: w, native, flags } if worker == 0xffff && native >> 8 == 0x60 => {
                line += &format!(" w{w}:poll {native:#06x}={flags}");
            }
            Record::NativeResult { worker: w, native, tag, value } if *w == worker => {
                let v = if *tag == 2 {
                    format!("{}", f32::from_bits(*value))
                } else {
                    format!("{}", *value as i32)
                };
                line += &format!(" {native:#06x}->{v}");
            }
            _ => {}
        }
    }
    Ok(())
}

fn archive_event_path(recorded: &str) -> Option<String> {
    let p = recorded.replace('\\', "/").to_ascii_lowercase();
    if let Some((_, rest)) = p.split_once("/master/") {
        let game = if p.contains("/ffx2/") { "ffx2" } else { "ffx" };
        return Some(format!("ffx_ps2/{game}/master/{rest}"));
    }
    // The short form the original keeps: `<aa>/<name>`.
    let (dir, name) = p.split_once('/')?;
    Some(format!("ffx_ps2/ffx/master/jppc/event/obj/{dir}/{name}/{name}.ebp"))
}

fn oracle_compare(install: &Install, dir: &std::path::Path, only: Option<usize>) -> Result<()> {
    use aerugo_formats::Game;
    use aerugo_script::recording::{event_runs, first_divergence, parse};
    use aerugo_sim::field::{Event, Input};
    let data = std::fs::read(dir.join("trace.bin"))?;
    let records = parse(&data)?;
    let runs = event_runs(&records);
    println!("{} records, {} event runs", records.len(), runs.len());
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    for (i, run) in runs.iter().enumerate() {
        if only.is_some_and(|o| o != i) {
            continue;
        }
        let Some(path) = archive_event_path(&run.path) else {
            println!("run {i}: event {} {:?}: path not understood", run.index, run.path);
            continue;
        };
        let game = if path.contains("/ffx2/") { Game::Ffx2 } else { Game::Ffx };
        let mut ev = match Event::load(&vfs, game, &path, aerugo_vfs::Language::English) {
            Ok(e) => e,
            Err(e) => {
                println!("run {i}: {path}: {e}");
                continue;
            }
        };
        // Start from the save image the original had and hand out its random numbers.
        let name = path.rsplit('/').next().unwrap_or(&path).trim_end_matches(".ebp").to_owned();
        let image = dir.join("saves").join(format!("{:08}-{name}.img", run.start_frame));
        if !image.exists() {
            println!("run {i}: no save image {}", image.display());
        }
        if let Ok(img) = std::fs::read(&image) {
            let n = img.len().min(ev.vm.memory.global[0].len());
            ev.vm.memory.global[0][..n].copy_from_slice(&img[..n]);
        }
        ev.state.rng_replay = run.random.iter().copied().collect();
        // Natives Aerugo lacks report what the original's did.
        ev.natives.replay = Some(run.replay.clone());
        // Menus last as long as the player keeps them open: replay them.
        ev.natives.remove(0x401D);
        ev.vm.trace = Some(Vec::new());
        // Compare as we go and stop at the first difference: a run that has gone astray can loop at the run
        // limit every frame, and its trace would not fit in memory.
        let mut ours = Vec::new();
        let mut found = None;
        // PLAYER_SIM: run the player controller on the recorded pad and compare its steps with the recorded ones.
        let mut control = aerugo_sim::control::PlayerControl::default();
        let mut step_errors: Vec<f32> = Vec::new();
        for f in 0..run.frames.len() {
            // Battles (recordings with battle records): the battle's own script runs in the original; skip
            // those frames, then end Aerugo's placeholder battle with it.
            let in_battle = run.battles.iter().rev().find(|b| b.0 as usize <= f).is_some_and(|b| b.1);
            if in_battle {
                ours.push(Vec::new());
                continue;
            }
            if ev.state.battle.is_some() && run.battles.iter().any(|b| b.1 && (b.0 as usize) < f) {
                ev.finish_battle();
            }
            if std::env::var("PLAYER_SIM").is_ok()
                && let Some(p) = ev.state.player
                && let Some(d) = run.inputs.get(f).filter(|d| d.len() >= 0x30)
            {
                let held = u32::from_le_bytes(d[0..4].try_into().expect("four bytes"));
                // Pad 0's stick: x at +0xE, y at +0xF (0 up), 0x80 centred. The controller reads it two frames
                // ahead of the recorded pad block.
                let d = run.inputs.get(f + 2).filter(|d| d.len() >= 0x30).unwrap_or(d);
                let stick =
                    [(f32::from(d[0x2e]) * 2.0 - 256.0) / 256.0, (128.0 - f32::from(d[0x2f])) * 2.0 / 256.0];
                let predicted = control
                    .tick(stick, held & 0x40 != 0, std::env::var("PLAYER_SWIM").is_ok())
                    .map_or(0.0, |p| p.step);
                let find = |k: usize| {
                    run.positions
                        .get(k)
                        .and_then(|ps| ps.iter().find(|q| usize::from(q.worker) == p))
                        .map(|q| q.pos)
                };
                if ev.state.user_control
                    && let (Some(a), Some(b)) = (find(f + 1), find(f + 2))
                {
                    let got = ((b[0] - a[0]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
                    step_errors.push(got - predicted);
                    if (got - predicted).abs() > 0.05 {
                        eprintln!("step f{f}: predicted {predicted:.4} recorded {got:.4}");
                    }
                }
            }
            // The player goes where the original's went (the stick is not replayed). The original moves
            // the player in its own turn, before other workers' triggers test it: by the position recorded at the
            // start of the next frame.
            if let Some(p) = ev.state.player
                && let Some(rec) =
                    run.positions.get(f + 1).and_then(|ps| ps.iter().find(|q| usize::from(q.worker) == p))
                && let Some(a) = ev.state.actors.get_mut(p)
            {
                a.pos = rec.pos;
                a.yaw = rec.yaw;
            }
            // Buttons the scripts saw pressed this frame (the original's pressed word).
            let pressed = run
                .inputs
                .get(f)
                .and_then(|d| d.get(0x10..0x14))
                .map_or(0, |b| u32::from_le_bytes(b.try_into().expect("four bytes")));
            let input = Input {
                confirm: pressed & 0x20 != 0,
                cancel: pressed & 0x40 != 0,
                alt: pressed & 0x80 != 0,
                ..Input::default()
            };
            ev.load_motion_frames(&vfs);
            if let Some(path) = ev.movie_needing_length() {
                let ticks = movie_ticks(&vfs, path).unwrap_or(0);
                ev.set_movie_frames(ticks);
            }
            ev.step(input);
            // PLAYER_DIR: the stick, the player's recorded step and the camera, per frame (for fitting the
            // player controller).
            if std::env::var("PLAYER_DIR").is_ok()
                && let Some(p) = ev.state.player
                && let Some(d) = run.inputs.get(f)
                && d.len() >= 0x30
            {
                let find = |k: usize| {
                    run.positions
                        .get(k)
                        .and_then(|ps| ps.iter().find(|q| usize::from(q.worker) == p))
                        .map(|q| q.pos)
                };
                if let (Some(a), Some(b)) = (find(f + 1), find(f + 2)) {
                    let cam = &ev.state.camera;
                    eprintln!(
                        "dir f{f} stick {} {} buttons {:02x}{:02x} step {:.4} {:.4} {:.4} eye {:?} focus {:?}",
                        d[0x2e],
                        d[0x2f],
                        d[0x23],
                        d[0x22],
                        b[0] - a[0],
                        b[1] - a[1],
                        b[2] - a[2],
                        cam.position,
                        cam.focus
                    );
                }
            }
            // CAMERA_CMP: the original's camera points next to Aerugo's script camera, per frame.
            if std::env::var("CAMERA_CMP").is_ok()
                && let Some(Some((a, b))) = run.cameras.get(f + 1)
            {
                let cam = &ev.state.camera;
                eprintln!(
                    "cam f{f} orig {:?} {:?} aerugo {:?} {:?}",
                    &a[..3],
                    &b[..3],
                    cam.position,
                    cam.focus
                );
            }
            if let Ok(spec) = std::env::var("COMPARE_DEBUG")
                && let Some((w, from, to)) = (|| {
                    let mut it = spec.split(',').map(|x| x.parse::<usize>().ok());
                    Some((it.next()??, it.next()??, it.next()??))
                })()
                && (from..=to).contains(&f)
            {
                let a = &ev.state.actors[w];
                eprintln!(
                    "f{f} player {:?} prev {:?} pos {:?}; w{w} pos {:?} yaw {} flags {:#x} {:?}",
                    ev.state.player,
                    ev.state.player_prev,
                    ev.state.player.map(|p| ev.state.actors[p].pos),
                    a.pos,
                    a.yaw,
                    a.flags,
                    a.trigger
                );
                for (k, win) in ev.state.windows.iter().enumerate().filter(|(_, w)| w.open || w.closing > 0) {
                    let len = win.message.and_then(|m| ev.state.messages.get(m)).map_or(0, |m| m.len());
                    eprintln!("  window {k}: {win:?} length {len}");
                }
            }
            ours.push(std::mem::take(ev.vm.trace.as_mut().expect("tracing")));
            if let Some(d) = first_divergence(&run.frames[f..=f], &ours[f..=f]) {
                found = Some(aerugo_script::recording::Divergence { frame: f, ..d });
                break;
            }
        }
        if !step_errors.is_empty() {
            let close = step_errors.iter().filter(|e| e.abs() <= 0.005).count();
            println!(
                "run {i}: player steps within 0.005 of the controller: {close} of {}",
                step_errors.len()
            );
        }
        let total: usize = run.frames.iter().map(Vec::len).sum();
        match found {
            None => println!("run {i}: {name}: {} frames, {total} instructions: identical", run.frames.len()),
            Some(d) => {
                let script = ev.vm.script.clone();
                let show = |t: Option<aerugo_script::TraceEvent>| match t {
                    Some(t) => {
                        let mnemonic = aerugo_formats::script::op_info(t.op).mnemonic;
                        let mut operand = t.operand.map(|v| format!("{v:#x}")).unwrap_or_default();
                        if mnemonic == "push.float"
                            && let Some(f) = t.operand.and_then(|v| {
                                script.workers.get(usize::from(t.worker))?.floats.get(usize::from(v))
                            })
                        {
                            operand += &format!(" ({f})");
                        }
                        format!("w{} l{} {:05x} {mnemonic} {operand}", t.worker, t.level, t.pc)
                    }
                    None => "(nothing)".into(),
                };
                let matched: usize = run.frames[..d.frame].iter().map(Vec::len).sum::<usize>() + d.index;
                let before = &run.frames[d.frame][d.index.saturating_sub(8)..d.index];
                for t in before {
                    println!("    {}", show(Some(*t)));
                }
                println!(
                    "run {i}: {name}: {} frames, {total} instructions; first difference at frame {} \
                     instruction {} ({matched} matched)\n  original: {}\n  aerugo:   {}",
                    run.frames.len(),
                    d.frame,
                    d.index,
                    show(d.expected),
                    show(d.got)
                );
                if let Some(t) = d.expected.or(d.got) {
                    // The same worker's recent instructions in the original, across frames.
                    let mut history = Vec::new();
                    'back: for f in (0..=d.frame).rev() {
                        let end = if f == d.frame { d.index } else { run.frames[f].len() };
                        for e in run.frames[f][..end].iter().rev().filter(|e| e.worker == t.worker) {
                            history.push((f, *e));
                            if history.len() == 24 {
                                break 'back;
                            }
                        }
                    }
                    println!("  worker {} in the original before that:", t.worker);
                    for (f, e) in history.iter().rev() {
                        println!("    f{f:<5} {}", show(Some(*e)));
                    }
                    println!("  and after:");
                    let after = (d.frame..run.frames.len())
                        .flat_map(|f| {
                            let start = if f == d.frame { d.index } else { 0 };
                            run.frames[f][start..].iter().map(move |e| (f, e))
                        })
                        .filter(|(_, e)| e.worker == t.worker)
                        .take(16);
                    for (f, e) in after {
                        println!("    f{f:<5} {}", show(Some(*e)));
                    }
                    describe_worker(&mut ev.vm, usize::from(t.worker), before);
                    if let Some(g) = d.got {
                        // Aerugo's side: the same worker's last instructions, with where they changed level.
                        println!("  worker {} in aerugo before that:", g.worker);
                        let mut history = Vec::new();
                        'ours: for f in (0..=d.frame).rev() {
                            let end = if f == d.frame { d.index } else { ours[f].len() };
                            for e in ours[f][..end].iter().rev().filter(|e| e.worker == g.worker) {
                                if history.last().is_none_or(|(_, l): &(usize, aerugo_script::TraceEvent)| {
                                    l.level != e.level || history.len() < 12
                                }) {
                                    history.push((f, *e));
                                }
                                if history.len() == 40 {
                                    break 'ours;
                                }
                            }
                        }
                        for (f, e) in history.iter().rev() {
                            println!("    f{f:<5} {}", show(Some(*e)));
                        }
                        if d.expected.is_some_and(|e| e.worker != g.worker) {
                            describe_worker(&mut ev.vm, usize::from(g.worker), &[]);
                        }
                    }
                }
            }
        }
        for (f, t, e) in &run.map_changes {
            println!("  map change at frame {f}: target {t} entrance {e}");
        }
    }
    Ok(())
}

/// Entries of worker `w` and the variables `recent` instructions touched, with their values in Aerugo now.
fn describe_worker(vm: &mut aerugo_script::Vm, w: usize, recent: &[aerugo_script::TraceEvent]) {
    use aerugo_formats::script::{Operand, op_info};
    let script = vm.script.clone();
    let Some(desc) = script.workers.get(w) else { return };
    let entries: Vec<String> = desc.entries.iter().map(|e| format!("{e:05x}")).collect();
    println!("  worker {w}: kind {} entries [{}]", desc.kind, entries.join(" "));
    if let Some(state) = vm.workers.get(w) {
        let pcs: Vec<String> =
            state.contexts.iter().enumerate().map(|(l, c)| format!("l{l}:{:05x}", c.pc)).collect();
        println!("  aerugo level {} contexts {}", state.level, pcs.join(" "));
        for r in &state.queue {
            println!(
                "    request from {:?} {:?} level {} entry {}{}",
                r.from,
                r.mode,
                r.level,
                r.entry,
                if r.running { " running" } else { "" }
            );
        }
    }
    let mut seen = Vec::new();
    for t in recent {
        if op_info(t.op).operand != Operand::Var {
            continue;
        }
        let Some(idx) = t.operand.map(usize::from) else { continue };
        if seen.contains(&idx) {
            continue;
        }
        seen.push(idx);
        let Some(var) = desc.vars.get(idx) else { continue };
        let place = vm.memory.place(var, 0, w, desc.data_areas, script.shared_data());
        let value = vm.memory.load_int(place, var.ty);
        println!(
            "  var {idx:#x}: area {} offset {:#x} {} x{} = {value} ({value:#x}) at {place:?}",
            var.area,
            var.offset,
            var.ty.name(),
            var.count
        );
    }
}

fn oracle_arity(dir: &std::path::Path) -> Result<()> {
    use aerugo_formats::Game;
    use aerugo_formats::script::{arity, native};
    let records = aerugo_script::recording::parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let measured = aerugo_script::recording::measured_arity(&records);
    let (mut agree, mut differ) = (0, 0);
    for (id, counts) in &measured {
        let table = arity(Game::Ffx, *id).map(|(i, f, _)| i32::from(i) + i32::from(f));
        let best = counts.iter().max_by_key(|(_, n)| **n).map(|(c, _)| *c);
        if std::env::var_os("ARITY_ALL").is_some() {
            println!("all {id:04x} {counts:?}");
        }
        if table == best && counts.len() == 1 {
            agree += 1;
        } else {
            differ += 1;
            println!(
                "{:04x} {:<32} table {table:?} measured {counts:?}",
                id,
                native(Game::Ffx, *id).display(*id)
            );
        }
    }
    println!("{} natives measured: {agree} agree with the table, {differ} differ", measured.len());
    Ok(())
}

fn oracle_motions(dir: &std::path::Path) -> Result<()> {
    use aerugo_formats::script::op_info;
    use aerugo_script::recording::{Record, parse};
    use std::collections::HashMap;
    #[derive(Default)]
    struct W {
        pushes: Vec<i64>,
        // (frame, motion, start, end, not-done polls)
        pending: Option<(u32, i64, i64, i64, u32)>,
    }
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let (mut frame, mut name) = (0u32, String::new());
    let mut ws: HashMap<u16, W> = HashMap::new();
    for r in &records {
        match r {
            Record::Event { path, .. } => {
                ws.clear();
                frame = 0;
                name = path.clone();
            }
            Record::Frame(_) => frame += 1,
            Record::Instruction { worker, op, operand, .. } => {
                let w = ws.entry(*worker).or_default();
                let m = op_info(*op).mnemonic;
                if m == "push.imm" {
                    w.pushes.push(i64::from(operand.unwrap_or(0) as i16));
                } else if m == "push.int" {
                    w.pushes.push(-1_000_000 - i64::from(operand.unwrap_or(0)));
                } else if m.starts_with("call") && operand == &Some(0x501A) {
                    let p = &w.pushes;
                    if p.len() >= 5 {
                        let n = p.len();
                        w.pending = Some((frame, p[n - 5], p[n - 2], p[n - 1], 0));
                    }
                    w.pushes.clear();
                } else {
                    w.pushes.clear();
                }
            }
            Record::NativePoll { worker, native: 0x501E | 0x5003, flags } => {
                let w = ws.entry(*worker).or_default();
                if let Some(p) = &mut w.pending {
                    if *flags == 0 {
                        p.4 += 1;
                    } else {
                        println!(
                            "{name} w{worker} f{}: motion {:#x} frames {}..{} waited {}",
                            p.0, p.1, p.2, p.3, p.4
                        );
                        w.pending = None;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A movie's length in field ticks (30 a second).
fn movie_ticks(vfs: &aerugo_vfs::Vfs, path: &str) -> Option<u32> {
    let demux = movie_demuxer(vfs, path).ok()?;
    Some((demux.duration_s * 30.0).round() as u32)
}

type ArchiveRead<'a> = Box<dyn FnMut(u64, usize) -> std::result::Result<Vec<u8>, String> + 'a>;

/// A movie file's demuxer, reading through the archives in 1 MiB chunks.
fn movie_demuxer<'a>(
    vfs: &'a aerugo_vfs::Vfs,
    path: &'a str,
) -> Result<aerugo_video::Demuxer<aerugo_video::Chunked<ArchiveRead<'a>>>> {
    let len = vfs.size(path).context("missing")?;
    let read: ArchiveRead<'a> =
        Box::new(move |at, n| vfs.read_range(path, at, n as u64).map_err(|e| e.to_string()));
    aerugo_video::Demuxer::open(aerugo_video::Chunked::new(len, 1 << 20, read))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn movie_info(install: &Install, movie: &str) -> Result<()> {
    use aerugo_formats::Game;
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let path = match movie.parse::<u32>() {
        Ok(n) => {
            let ev = aerugo_sim::field::Event::load(
                &vfs,
                Game::Ffx,
                "ffx_ps2/ffx/master/jppc/event/obj/bj/bjyt0000/bjyt0000.ebp",
                aerugo_vfs::Language::English,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            ev.state.videos.get(&n).cloned().ok_or_else(|| anyhow::anyhow!("movie {n} is not in the list"))?
        }
        Err(_) => movie.to_owned(),
    };
    let mut demux = movie_demuxer(&vfs, &path)?;
    println!("{path}: {:.3} s, {} ticks", demux.duration_s, movie_ticks(&vfs, &path).unwrap_or(0));
    let video = demux.tracks.iter().find(|t| t.kind == 1).map(|t| t.number);
    let mut shown = 0;
    while let Some(p) = demux.next_packet().map_err(|e| anyhow::anyhow!("{e}"))? {
        if Some(p.track) == video {
            println!("  frame at {:.4} s", p.time_ns as f64 / 1e9);
            shown += 1;
            if shown == 6 {
                break;
            }
        }
    }
    Ok(())
}

fn oracle_frames(dir: &std::path::Path, run: usize, from: u32, to: u32) -> Result<()> {
    use aerugo_script::recording::{Record, parse};
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let (mut index, mut frame) = (None::<usize>, 0u32);
    let mut line: Vec<String> = Vec::new();
    let flush = |frame: u32, line: &mut Vec<String>| {
        if (from..=to).contains(&frame) {
            println!("f{frame}: {}", line.join(" "));
        }
        line.clear();
    };
    for r in &records {
        match r {
            Record::Event { .. } => {
                index = Some(index.map_or(0, |i| i + 1));
                frame = 0;
                line.clear();
            }
            Record::Frame(_) if index == Some(run) => {
                flush(frame, &mut line);
                frame += 1;
            }
            Record::Instruction { worker, level, pc, op, operand, .. } if index == Some(run) => {
                // OPS: also print instructions whose mnemonic contains this text.
                if let Ok(filter) = std::env::var("OPS")
                    && (from..=to).contains(&frame)
                    && aerugo_formats::script::op_info(*op).mnemonic.contains(filter.as_str())
                {
                    println!(
                        "  f{frame} w{worker} l{level} {pc:05x} {} {operand:?}",
                        aerugo_formats::script::op_info(*op).mnemonic
                    );
                }
                let w = format!("{worker}:{level}");
                if line.last() != Some(&w) {
                    line.push(w);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn oracle_random(dir: &std::path::Path) -> Result<()> {
    use aerugo_script::recording::{Record, parse};
    use std::collections::BTreeMap;
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let (mut name, mut frame) = (String::new(), 0u32);
    let mut per: BTreeMap<(String, u8), (u32, Vec<String>)> = BTreeMap::new();
    let mut order = Vec::new();
    for r in &records {
        match r {
            Record::Event { path, .. } => {
                name = format!("{}#{}", path.rsplit('/').next().unwrap_or(path), order.len());
                order.push(name.clone());
                frame = 0;
            }
            Record::Frame(_) => frame += 1,
            Record::Random { stream, value } => {
                let e = per.entry((name.clone(), *stream)).or_default();
                e.0 += 1;
                if e.1.len() < 4 {
                    e.1.push(format!("f{frame}:{value:#x}"));
                }
            }
            _ => {}
        }
    }
    for ((event, stream), (n, first)) in per {
        println!("{event} stream {stream}: {n} ({})", first.join(" "));
    }
    Ok(())
}

fn oracle_missing(dir: &std::path::Path) -> Result<()> {
    use aerugo_formats::Game;
    use aerugo_formats::script::op_info;
    use aerugo_script::recording::{Record, parse};
    use std::collections::BTreeMap;
    let mut natives = aerugo_script::Natives::<aerugo_sim::field::FieldState>::new(Game::Ffx);
    aerugo_sim::field::register(&mut natives);
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let mut name = String::new();
    let mut per_event: BTreeMap<String, BTreeMap<u16, u32>> = BTreeMap::new();
    let mut all: BTreeMap<u16, (u32, std::collections::BTreeSet<String>)> = BTreeMap::new();
    for r in &records {
        match r {
            Record::Event { path, .. } => name = path.rsplit('/').next().unwrap_or(path).to_owned(),
            Record::Instruction { op, operand: Some(id), .. }
                if op_info(*op).mnemonic.starts_with("call") && !natives.implemented(*id) =>
            {
                *per_event.entry(name.clone()).or_default().entry(*id).or_default() += 1;
                let e = all.entry(*id).or_default();
                e.0 += 1;
                e.1.insert(name.clone());
            }
            _ => {}
        }
    }
    for (event, ids) in &per_event {
        let list: Vec<String> = ids.iter().map(|(id, n)| format!("{id:04x}x{n}")).collect();
        println!("{event}: {}", list.join(" "));
    }
    let mut v: Vec<_> = all.into_iter().collect();
    v.sort_by_key(|(id, (n, _))| (std::cmp::Reverse(*n), *id));
    for (id, (n, events)) in v {
        println!("{id:04x} {n:>8} {}", events.into_iter().collect::<Vec<_>>().join(","));
    }
    Ok(())
}

fn kernel_text(install: &Install, table: &str, first: u16, last: u16) -> Result<()> {
    use aerugo_formats::text::{Charset, charset_path, decode, string_at, to_display};
    use aerugo_formats::{Game, KernelTable};
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let lang = aerugo_vfs::Language::English;
    let path = format!("ffx_ps2/ffx/master/jppc/battle/kernel/{table}.bin");
    let t = KernelTable::parse(vfs.read_localized(&path, lang)?)?;
    let cs = Charset::parse(&vfs.read(&charset_path(Game::Ffx, lang))?)?;
    println!("{} records of {} bytes", t.len(), t.record_size());
    for id in first..=last {
        let Some(r) = t.record(u32::from(id)) else { continue };
        let hex: Vec<String> = r.bytes.iter().map(|b| format!("{b:02x}")).collect();
        println!("{id:#06x}: {}", hex.join(" "));
        for off in (0..r.bytes.len().min(0x40)).step_by(4) {
            if let Some(s) = t.text(&r, off).filter(|s| !s.is_empty()) {
                let _ = string_at;
                println!("  +{off:#04x}: {}", to_display(&decode(s, &cs)));
            }
        }
    }
    Ok(())
}

fn native_args(install: &Install, native: u16) -> Result<()> {
    use aerugo_formats::script::{Script, is_script_path};
    use std::collections::BTreeMap;
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let paths: Vec<String> = vfs
        .archives()
        .filter(|(id, _)| *id == Some(ArchiveId::Ffx))
        .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
        .filter(|p| p.ends_with(".ebp") && is_script_path(p) && p.contains("/jppc/event/obj/"))
        .collect();
    let mut seen: BTreeMap<String, (u32, std::collections::BTreeSet<String>)> = BTreeMap::new();
    for path in &paths {
        let Ok(script) = Script::parse(&vfs.read(path)?) else { continue };
        let event = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".ebp").to_owned();
        for (wi, w) in script.workers.iter().enumerate() {
            let mut pushes: Vec<String> = Vec::new();
            for insn in script.walk(wi) {
                let m = insn.info().mnemonic;
                let arg = insn.operand.unwrap_or(0);
                match m {
                    "push.imm" => pushes.push((arg as i16).to_string()),
                    "push.int" => {
                        pushes.push(w.ints.get(usize::from(arg)).map_or("?".into(), |v| format!("{v:#x}")))
                    }
                    "push.float" => {
                        pushes.push(w.floats.get(usize::from(arg)).map_or("?".into(), |v| format!("{v}")))
                    }
                    _ if m.starts_with("push") => pushes.push(m.to_owned()),
                    _ if m.starts_with("call") && arg == native => {
                        let key = pushes.iter().rev().take(4).rev().cloned().collect::<Vec<_>>().join(", ");
                        let e = seen.entry(key).or_default();
                        e.0 += 1;
                        e.1.insert(event.clone());
                        pushes.clear();
                    }
                    _ => pushes.clear(),
                }
            }
        }
    }
    let mut v: Vec<_> = seen.into_iter().collect();
    v.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (args, (n, events)) in v {
        let list: Vec<_> = events.iter().take(8).cloned().collect();
        println!("{n:>5}  [{args}]  {}{}", list.join(","), if events.len() > 8 { ",…" } else { "" });
    }
    Ok(())
}

fn parse_hex_u16(s: &str) -> std::result::Result<u16, String> {
    u16::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

fn oracle_natives(dir: &std::path::Path, natives: &[u16]) -> Result<()> {
    use aerugo_formats::script::op_info;
    use aerugo_script::recording::{Record, parse};
    use std::collections::HashMap;
    // Per worker: values pushed since the last call, and the call waiting for a result (native, frame, args,
    // polls).
    #[derive(Default)]
    struct W {
        pushes: Vec<i64>,
        pending: Option<(u16, u32, Vec<i64>, u32)>,
    }
    let records = parse(&std::fs::read(dir.join("trace.bin"))?)?;
    let (mut frame, mut name) = (0u32, String::new());
    let mut ws: HashMap<u16, W> = HashMap::new();
    let report = |name: &str, worker: u16, p: &(u16, u32, Vec<i64>, u32), result: &str| {
        println!("{name} f{} w{worker} {:04x}({:?}) polls {} -> {result}", p.1, p.0, p.2, p.3);
    };
    for r in &records {
        match r {
            Record::Event { path, .. } => {
                ws.clear();
                frame = 0;
                name = path.rsplit('/').next().unwrap_or(path).to_owned();
            }
            Record::Frame(_) => frame += 1,
            Record::Instruction { worker, op, operand, .. } => {
                let w = ws.entry(*worker).or_default();
                let m = op_info(*op).mnemonic;
                if m == "push.imm" {
                    w.pushes.push(i64::from(operand.unwrap_or(0) as i16));
                } else if m == "push.int" {
                    // Pushed from the int table: shown as -1000000 - index.
                    w.pushes.push(-1_000_000 - i64::from(operand.unwrap_or(0)));
                } else if m.starts_with("call") {
                    if let Some(p) = w.pending.take() {
                        report(&name, *worker, &p, "?");
                    }
                    if let Some(id) = operand.filter(|id| natives.contains(id)) {
                        w.pending = Some((id, frame, std::mem::take(&mut w.pushes), 0));
                    }
                    w.pushes.clear();
                } else if !m.starts_with("push") {
                    w.pushes.clear();
                }
            }
            Record::NativePoll { worker, native, flags } if natives.contains(native) => {
                let w = ws.entry(*worker).or_default();
                if let Some(p) = &mut w.pending {
                    if *flags == 0 {
                        p.3 += 1;
                    } else if *flags & 1 == 0 {
                        // Done without a result.
                        let p = w.pending.take().unwrap();
                        report(&name, *worker, &p, &format!("poll {flags}"));
                    }
                }
            }
            Record::NativeResult { worker, native, tag, value } if natives.contains(native) => {
                let w = ws.entry(*worker).or_default();
                if let Some(p) = w.pending.take() {
                    let v = if *tag == 2 {
                        format!("{}", f32::from_bits(*value))
                    } else {
                        format!("{}", *value as i32)
                    };
                    report(&name, *worker, &p, &v);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn field_coverage(install: &Install, frames: u32) -> Result<()> {
    use aerugo_formats::Game;
    use aerugo_sim::field::{Event, Input};
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let paths: Vec<String> = vfs
        .archives()
        .filter(|(id, _)| *id == Some(ArchiveId::Ffx))
        .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
        .filter(|p| p.ends_with(".ebp") && p.contains("/jppc/event/obj/"))
        .collect();
    let mut total = aerugo_script::natives::Coverage::default();
    for path in &paths {
        let Ok(mut ev) = Event::load(&vfs, Game::Ffx, path, aerugo_vfs::Language::English) else { continue };
        for t in 0..frames {
            if ev.state.player.is_none() && ev.initialized() {
                ev.state.player = ev.state.actors.iter().position(|a| a.model.is_some_and(|m| m >> 12 == 0));
            }
            ev.load_motion_frames(&vfs);
            if let Some(path) = ev.movie_needing_length() {
                let ticks = movie_ticks(&vfs, path).unwrap_or(0);
                ev.set_movie_frames(ticks);
            }
            ev.step(Input { confirm: t % 30 == 29, ..Input::default() });
            if ev.open_menu().is_some() {
                ev.close_menu();
            }
            if ev.running_battle().is_some() {
                ev.finish_battle();
            }
        }
        for (id, n) in &ev.natives.coverage.calls {
            *total.calls.entry(*id).or_default() += n;
        }
        total.missing.extend(ev.natives.coverage.missing.iter().copied());
    }
    println!("FFX field: {} events\n{}", paths.len(), total.report(Game::Ffx));
    // Every missing native, most called first.
    let mut missing: Vec<(u16, u64)> =
        total.missing.iter().map(|id| (*id, total.calls.get(id).copied().unwrap_or(0))).collect();
    missing.sort_by_key(|&(id, n)| (std::cmp::Reverse(n), id));
    for (id, n) in missing {
        println!(
            "all-missing {id:04x} {:<36} {n}",
            aerugo_formats::script::native(Game::Ffx, id).display(id)
        );
    }
    Ok(())
}

fn native_coverage(install: &Install, frames: u64) -> Result<()> {
    use aerugo_formats::Game;
    use aerugo_formats::script::{self, Script};
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    for (archive, game) in [(ArchiveId::Ffx, Game::Ffx), (ArchiveId::Ffx2, Game::Ffx2)] {
        let paths: Vec<String> = vfs
            .archives()
            .filter(|(id, _)| *id == Some(archive))
            .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
            .filter(|p| script::is_script_path(p))
            .collect();
        let mut natives = aerugo_script::Natives::<()>::new(game);
        for path in &paths {
            let Ok(s) = Script::parse(&vfs.read(path)?) else { continue };
            let mut vm = aerugo_script::Vm::new(std::sync::Arc::new(s));
            for _ in 0..frames {
                vm.frame(&mut natives.host(&mut ()));
            }
        }
        println!("{}: {} scripts\n{}", game.name(), paths.len(), natives.coverage.report(game));
    }
    Ok(())
}

fn effect_census(install: &Install, top: usize) -> Result<()> {
    use aerugo_formats::effect::{self, read_effect};
    use aerugo_formats::phyre::Cluster;
    use std::collections::HashMap;
    let mut uses: HashMap<(aerugo_formats::Game, String), (usize, String)> = HashMap::new();
    let (mut models, mut segments) = (0usize, 0usize);
    for archive in open_all(install)? {
        for e in archive.entries().iter().filter(|e| e.path.ends_with(".dae.phyre")) {
            let game = if e.path.starts_with("ffx-2_data") {
                aerugo_formats::Game::Ffx2
            } else {
                aerugo_formats::Game::Ffx
            };
            let Ok(c) = Cluster::parse(archive.read(e)?) else { continue };
            let Ok(m) = aerugo_formats::mesh::read_model(&c) else { continue };
            models += 1;
            for mesh in &m.meshes {
                for s in &mesh.segments {
                    let Some(mat) = mesh.materials.get(s.material as usize) else { continue };
                    segments += 1;
                    let entry = uses.entry((game, mat.effect.clone())).or_insert((0, e.path.clone()));
                    entry.0 += 1;
                }
            }
        }
    }
    let mut ranked: Vec<_> = uses.into_iter().collect();
    ranked.sort_by_key(|r| std::cmp::Reverse(r.1.0));
    println!("{models} models, {segments} segments, {} effects", ranked.len());
    let vfs = {
        let mut v = aerugo_vfs::Vfs::new();
        v.mount_install(install)?;
        v
    };
    for ((game, id), (n, example)) in ranked.iter().take(top) {
        let state = effect::effect_cluster_path(*game, id)
            .and_then(|p| vfs.read(&p).ok())
            .and_then(|d| Cluster::parse(d).ok())
            .and_then(|c| read_effect(&c));
        let summary = match &state {
            Some(fx) => {
                let kinds: Vec<&str> = fx.scene_passes.iter().map(|p| p.kind.as_str()).collect();
                format!(
                    "{:?} main={:?} [{}]",
                    fx.main_state(),
                    fx.main_pass().map(|p| &p.kind),
                    kinds.join(",")
                )
            }
            None => "unresolved".into(),
        };
        println!("{n:7} {} {id}  e.g. {example}\n        {summary}", game.name());
    }
    Ok(())
}

fn model_census(install: &Install) -> Result<()> {
    use aerugo_formats::phyre::{Cluster, Pointer};
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut counts_examples = 0;
    let mut add = |k: String| *counts.entry(k).or_default() += 1;
    for archive in open_all(install)? {
        for e in archive.entries().iter().filter(|e| e.path.ends_with(".dae.phyre")) {
            let Ok(c) = Cluster::parse(archive.read(e)?) else { continue };
            for s in c.objects("PVertexStream") {
                let sem = match c.pointer(s, "m_renderDataType") {
                    Some(Pointer::User(u)) => c.user_string(u),
                    _ => "?".into(),
                };
                add(format!("stream {sem} type {}", c.u32(s, "m_type").unwrap_or(999)));
            }
            for b in c.objects("PDataBlock") {
                let streams = c.array_count(b, "m_streams").unwrap_or(0);
                add(format!("block streams {streams}"));
                if streams == 1
                    && let Some(s) = c.array_targets(b, "m_streams").first()
                {
                    {
                        let sem = match c.pointer(*s, "m_renderDataType") {
                            Some(Pointer::User(u)) => c.user_string(u),
                            _ => "?".into(),
                        };
                        add(format!(
                            "layout {sem} type {} stride {}",
                            c.u32(*s, "m_type").unwrap_or(999),
                            c.u32(b, "m_stride").unwrap_or(0)
                        ));
                    }
                }
            }
            for g in c.objects("PMeshSegment") {
                let n = c.u32(g, "m_indexData.m_elementCount").unwrap_or(0);
                add(format!(
                    "segment primitive {} index type {} count%3 {}",
                    c.u32(g, "m_primitiveType").unwrap_or(999),
                    c.u32(g, "m_indexData.m_type").unwrap_or(999),
                    n % 3
                ));
                let size = c.u32(g, "m_indexData.m_dataSize").unwrap_or(0);
                if let Some(per) = size.checked_div(n) {
                    add(format!("index bytes/element {per}"));
                }
            }
            for u in &c.user_fixups {
                let t = c.namespace.type_name(u.type_id).to_owned();
                if t == "PAssetReferenceImport" {
                    let d = &c.data()[u.data.clone()];
                    add(format!("import fixup len {} first {}", d.len(), d.first().copied().unwrap_or(0)));
                }
            }
            add(format!("instances {}", c.objects("PMeshInstance").len().min(9)));
            match aerugo_formats::mesh::read_model(&c) {
                Ok(m) => {
                    add("model ok".into());
                    for mesh in &m.meshes {
                        let Some(sk) = aerugo_formats::skeleton::read_skeleton(&c, mesh.mesh) else {
                            continue;
                        };
                        let world = sk.world(&sk.default_pose);
                        let worst = mesh
                            .segments
                            .iter()
                            .flat_map(|s| sk.palette(&world, s))
                            .map(|p| aerugo_formats::skeleton::identity_error(&p))
                            .fold(0.0f32, f32::max);
                        let bucket = if worst < 1e-3 {
                            "<1e-3"
                        } else if worst < 1e-2 {
                            "<1e-2"
                        } else if worst < 1e-1 {
                            "<0.1"
                        } else {
                            ">=0.1"
                        };
                        add(format!("skin bind error {bucket}"));
                        if worst >= 0.1 && counts_examples < 5 {
                            counts_examples += 1;
                            println!("  bind error {worst:.3}: {}", e.path);
                        }
                        // Weights should sum to one.
                        for s in &mesh.segments {
                            if let Some(ws) = s.stream("SkinWeights") {
                                let w = aerugo_formats::mesh::stream_floats(&c, ws);
                                let bad = w
                                    .chunks_exact(4)
                                    .filter(|v| (v.iter().sum::<f32>() - 1.0).abs() > 0.01)
                                    .count();
                                add(if bad == 0 {
                                    "weights sum to 1".into()
                                } else {
                                    "weights do NOT sum to 1".into()
                                });
                            }
                        }
                    }
                    for mesh in &m.meshes {
                        for mat in &mesh.materials {
                            add(format!("material textures {}", mat.textures.len().min(5)));
                            for t in &mat.textures {
                                let path = aerugo_formats::mesh::texture_cluster_path(&e.path, &t.asset);
                                add(if archive.find(&path).is_some() {
                                    "texture found".into()
                                } else {
                                    format!("texture missing (param {})", t.parameter)
                                });
                            }
                        }
                        for s in &mesh.segments {
                            if s.positions().is_none() {
                                add("segment without positions".into());
                            }
                            if (s.material as usize) >= mesh.materials.len() {
                                add("segment material out of range".into());
                            }
                        }
                    }
                }
                Err(err) => {
                    add(format!("model error {err}"));
                }
            }
        }
    }
    for (k, v) in counts {
        println!("{v:>8}  {k}");
    }
    Ok(())
}

fn motion_fit(install: &Install, model: &str, mgrp: &str) -> Result<()> {
    use aerugo_formats::motion::MotionGroup;
    use aerugo_formats::phyre::Cluster;
    use glam::{EulerRot, Mat3, Mat4, Vec3};
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let c = Cluster::parse(vfs.read(model)?)?;
    let m = aerugo_formats::mesh::read_model(&c)?;
    let sk = m
        .meshes
        .iter()
        .filter_map(|mesh| aerugo_formats::skeleton::read_skeleton(&c, mesh.mesh))
        .max_by_key(|s| s.len())
        .context("no skeleton")?;
    let group = MotionGroup::parse(&vfs.read(mgrp)?)?;
    let motion = group.motions.first().context("no motion")?;
    println!("motion: {} frames, {} joints; HD skeleton {} joints", motion.frames, motion.joints, sk.len());
    let pairs: Vec<(usize, usize)> = sk
        .names
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n.strip_prefix("SkeletonNode_")?.parse::<usize>().ok().map(|ps2| (i, ps2)))
        .filter(|&(_, ps2)| ps2 < motion.joints as usize)
        .collect();
    // Translation scale.
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for &(hd, ps2) in &pairs {
        let l = &sk.default_pose[hd];
        let t = Vec3::new(l[12], l[13], l[14]);
        let r = Vec3::from(motion.joint(ps2, 0).translation);
        num += f64::from(t.dot(r));
        den += f64::from(r.dot(r));
    }
    let k = if den > 0.0 { num / den } else { 0.0 } as f32;
    let terr: f32 = pairs
        .iter()
        .map(|&(hd, ps2)| {
            let l = &sk.default_pose[hd];
            (Vec3::new(l[12], l[13], l[14]) - Vec3::from(motion.joint(ps2, 0).translation) * k).length()
        })
        .sum::<f32>()
        / pairs.len().max(1) as f32;
    println!("translation scale {k:e}, mean error {terr:.4} over {} joints", pairs.len());
    let orders = [
        ("XYZ", EulerRot::XYZ),
        ("XZY", EulerRot::XZY),
        ("YXZ", EulerRot::YXZ),
        ("YZX", EulerRot::YZX),
        ("ZXY", EulerRot::ZXY),
        ("ZYX", EulerRot::ZYX),
    ];
    for (name, order) in orders {
        for sign in [1.0f32, -1.0] {
            let err: f32 = pairs
                .iter()
                .map(|&(hd, ps2)| {
                    let l = Mat4::from_cols_array(&sk.default_pose[hd]);
                    let hd_rot = Mat3::from_mat4(l);
                    let hd_rot = Mat3::from_cols(
                        hd_rot.x_axis.normalize_or_zero(),
                        hd_rot.y_axis.normalize_or_zero(),
                        hd_rot.z_axis.normalize_or_zero(),
                    );
                    let p = motion.joint(ps2, 0).rotation;
                    // EulerRot applies the first-named axis first in glam's convention for from_euler(a, b, c).
                    let (a, b, cc) = match name {
                        "XYZ" => (p[0], p[1], p[2]),
                        "XZY" => (p[0], p[2], p[1]),
                        "YXZ" => (p[1], p[0], p[2]),
                        "YZX" => (p[1], p[2], p[0]),
                        "ZXY" => (p[2], p[0], p[1]),
                        _ => (p[2], p[1], p[0]),
                    };
                    let r = Mat3::from_euler(order, a * sign, b * sign, cc * sign);
                    (0..3).map(|i| (hd_rot.col(i) - r.col(i)).length()).sum::<f32>()
                })
                .sum::<f32>()
                / pairs.len().max(1) as f32;
            println!("  order {name} sign {sign:+}: mean error {err:.4}");
        }
    }
    Ok(())
}

fn check_motions(install: &Install) -> Result<()> {
    use aerugo_formats::motion::MotionGroup;
    let (mut ok, mut motions, mut failed) = (0, 0, Vec::new());
    for archive in open_all(install)? {
        for e in archive.entries().iter().filter(|e| e.path.ends_with(".mgrp") && e.size > 16) {
            match MotionGroup::parse(&archive.read(e)?) {
                Ok(g) => {
                    ok += 1;
                    motions += g.motions.len();
                }
                Err(err) => failed.push(format!("{}: {err}", e.path)),
            }
        }
    }
    println!("{ok} motion groups, {motions} motions, {} failed", failed.len());
    for f in failed.iter().take(20) {
        println!("  {f}");
    }
    Ok(())
}

fn ffmpeg_decode(wav: &[u8], scratch: &std::path::Path) -> Result<Vec<i16>> {
    let input = scratch.join("aerugo-fsb-ref.wav");
    std::fs::write(&input, wav)?;
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-c:a", "adpcm_ima_xbox", "-i"])
        .arg(&input)
        .args(["-f", "s16le", "-"])
        .output()
        .context("running ffmpeg")?;
    if !out.status.success() {
        bail!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(out.stdout.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect())
}

fn fsb_compare(install: &Install, filter: &str, every: usize, per_bank: usize) -> Result<()> {
    use aerugo_formats::fsb::{Bank, wav_xbox_ima};
    let scratch = std::env::temp_dir();
    let (mut checked, mut identical, mut frames_total) = (0usize, 0usize, 0usize);
    let mut worst = (0i32, String::new());
    for archive in open_all(install)? {
        let banks: Vec<_> = archive
            .entries()
            .iter()
            .filter(|e| e.path.ends_with(".fsb") && e.path.contains(filter))
            .step_by(every)
            .collect();
        for e in banks {
            let data = archive.read(e)?;
            let bank = Bank::parse(&data)?;
            let n = bank.samples.len();
            for s in bank.samples.iter().step_by((n / per_bank.max(1)).max(1)).take(per_bank) {
                let ours = bank.decode(&data, s)?;
                let reference =
                    ffmpeg_decode(&wav_xbox_ima(s.rate, s.channels, &data[s.data.clone()]), &scratch)?;
                let len = ours.len().min(reference.len());
                let diff = ours[..len]
                    .iter()
                    .zip(&reference[..len])
                    .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                    .max()
                    .unwrap_or(0);
                checked += 1;
                frames_total += len;
                if diff == 0 && reference.len() >= ours.len() {
                    identical += 1;
                } else if diff >= worst.0 {
                    worst = (
                        diff,
                        format!(
                            "{} #{} ({}): ours {} values, ffmpeg {}",
                            e.path,
                            s.name,
                            s.frames,
                            ours.len(),
                            reference.len()
                        ),
                    );
                }
            }
        }
    }
    println!("{checked} samples ({frames_total} values) compared, {identical} identical");
    if identical != checked {
        println!("largest difference {}: {}", worst.0, worst.1);
        bail!("decoders differ");
    }
    Ok(())
}

fn fsb_export(install: &Install, bank: &str, index: usize, out: &std::path::Path) -> Result<()> {
    use aerugo_formats::fsb::{Bank, wav_pcm16};
    let mut vfs = aerugo_vfs::Vfs::new();
    vfs.mount_install(install)?;
    let data = vfs.read(bank)?;
    let b = Bank::parse(&data)?;
    let s = b.samples.get(index).context("no such sample")?;
    std::fs::write(out, wav_pcm16(s.rate, s.channels, &b.decode(&data, s)?))?;
    println!("{}: {} Hz, {} channels, {} frames, loop {:?}", s.name, s.rate, s.channels, s.frames, s.looping);
    Ok(())
}
