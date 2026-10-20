//! `fossickd`: the Fossick indexing daemon.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use fossick_proto::ServerMode;
use fossickd::config::{Config, Paths};
use fossickd::events::Cmd;
use fossickd::server::Server;
use fossickd::state::Shared;
use fossickd::writer::Writer;
use fossickd::{log, mounts, sys};

/// Keeps a live index of every file name and answers searches over a Unix socket.
#[derive(Parser, Debug)]
#[command(name = "fossickd", version, about)]
struct Args {
    /// Run as a per-user daemon: index $HOME, listen on $XDG_RUNTIME_DIR/fossick/socket.
    #[arg(long)]
    user: bool,
    /// Configuration file (default: /etc/fossick/fossickd.toml, or
    /// ~/.config/fossick/fossickd.toml with --user).
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Directory for the snapshot and run history.
    #[arg(long, value_name = "DIR")]
    state_dir: Option<PathBuf>,
    /// Socket path.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    /// Print debug messages.
    #[arg(long)]
    debug: bool,
}

fn main() {
    if let Err(e) = run() {
        log::error(&format!("{e:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    log::set_debug(args.debug);
    // Signals are handled by one thread; block them everywhere else (threads inherit the mask).
    let signals = {
        use nix::sys::signal::{SigSet, Signal};
        let mut s = SigSet::empty();
        s.add(Signal::SIGTERM);
        s.add(Signal::SIGINT);
        s.add(Signal::SIGHUP);
        s.thread_block().context("blocking signals")?;
        s
    };
    let activated = sys::listen_fds();
    sys::limit_malloc_arenas(2);
    let mode = if args.user {
        ServerMode::User
    } else {
        ServerMode::System
    };
    let mut paths = if args.user {
        Paths::user()?
    } else {
        Paths::system()
    };
    if let Some(c) = args.config {
        paths.config = c;
    }
    if let Some(d) = args.state_dir {
        paths.state_dir = d;
    }
    let config = Config::load(&paths.config)?;
    if let Some(s) = args.socket.or_else(|| config.server.socket.clone()) {
        paths.socket = s;
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&paths.state_dir)
        .with_context(|| format!("creating {}", paths.state_dir.display()))?;
    let listener = match activated.into_iter().next() {
        Some(fd) => {
            log::info("using the socket passed by systemd");
            UnixListener::from(fd)
        }
        None => bind(&paths.socket, mode)?,
    };
    log::info(&format!(
        "fossickd {} starting in {} mode (socket {}, state {})",
        env!("CARGO_PKG_VERSION"),
        if args.user { "user" } else { "system" },
        paths.socket.display(),
        paths.state_dir.display()
    ));
    let shared = Shared::new(mode, config, paths);
    let (tx, rx) = mpsc::channel::<Cmd>();
    let writer = {
        let shared = shared.clone();
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("fossick-writer".into())
            .spawn(move || Writer::new(shared, rx, tx).run())?
    };
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("fossick-mounts".into())
            .spawn(move || watch_mounts(&tx))?;
    }
    {
        let server = Server::new(shared.clone(), tx.clone());
        std::thread::Builder::new()
            .name("fossick-accept".into())
            .spawn(move || server.serve(&listener))?;
    }
    // SIGHUP re-reads the configuration; SIGTERM and SIGINT stop the writer (it writes a
    // final snapshot).
    loop {
        let sig = signals.wait().context("waiting for signals")?;
        if sig == nix::sys::signal::Signal::SIGHUP {
            match Config::load(&shared.paths.config) {
                Ok(cfg) => {
                    let _ = tx.send(Cmd::Reload(Box::new(cfg)));
                }
                Err(e) => log::warn(&format!("keeping the old configuration: {e}")),
            }
            continue;
        }
        log::info(&format!("received {sig}, shutting down"));
        break;
    }
    shared.request_shutdown();
    let _ = tx.send(Cmd::Shutdown);
    let _ = writer.join();
    if mode == ServerMode::User || shared.paths.socket.starts_with("/run") {
        let _ = std::fs::remove_file(&shared.paths.socket);
    }
    Ok(())
}

/// Binds the socket: world-connectable in system mode (authorization comes from peer
/// credentials), owner-only in user mode.
fn bind(path: &Path, mode: ServerMode) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(if mode == ServerMode::User {
                0o700
            } else {
                0o755
            })
            .create(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    if UnixStream::connect(path).is_ok() {
        bail!(
            "another fossickd is already listening on {}",
            path.display()
        );
    }
    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    let perm = if mode == ServerMode::User {
        0o600
    } else {
        0o666
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(perm))?;
    Ok(listener)
}

/// Tells the writer whenever the mount table changes.
fn watch_mounts(tx: &mpsc::Sender<Cmd>) {
    let Ok(file) = std::fs::File::open("/proc/self/mountinfo") else {
        log::warn("cannot watch /proc/self/mountinfo: mounts are only read at startup");
        return;
    };
    loop {
        match mounts::wait_for_mount_change(&file, std::time::Duration::from_secs(3600)) {
            Ok(true) => {
                // Mounting often comes in bursts (a disk with several partitions).
                std::thread::sleep(std::time::Duration::from_millis(300));
                if tx.send(Cmd::MountsChanged).is_err() {
                    return;
                }
            }
            Ok(false) => {}
            Err(e) => {
                log::warn(&format!("mount watcher stopped: {e}"));
                return;
            }
        }
    }
}
