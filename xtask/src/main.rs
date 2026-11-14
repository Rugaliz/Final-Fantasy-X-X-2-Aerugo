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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let install = find_install(cli.game_dir)?;
    match cli.command {
        Command::CheckGame => check_game(&install),
        Command::VerifyArchives { threads } => verify(&install, threads),
        Command::List { filter } => list(&install, filter.as_deref()),
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
