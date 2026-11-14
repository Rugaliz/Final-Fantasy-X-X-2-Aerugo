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
