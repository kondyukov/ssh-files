mod action;
mod app;
mod bench;
mod cli;
mod clipboard;
mod config;
mod executor;
mod file_tree;
mod input;
mod keymap;
mod source;
mod ssh;
mod theme;
mod transfer;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::prelude::*;
use std::io::stdout;
use std::path::PathBuf;

use app::App;
use cli::ConnectionInfo;
use ssh::SftpClientShared;
use theme::{ColorSupport, Theme};

#[derive(Parser, Debug)]
#[command(name = "ssh-files")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// SSH target: [user@]host[:port][:path]
    #[arg(required_unless_present_any = ["local", "dual_remote", "virtual_relay"])]
    target: Option<String>,

    /// Browse the local filesystem in both panes; DIR opens in the right
    /// pane (defaults to the current directory)
    #[arg(
        long,
        value_name = "DIR",
        num_args = 0..=1,
        default_missing_value = ".",
        conflicts_with = "target"
    )]
    local: Option<String>,

    /// Dual-remote browsing: both panes on remote hosts with direct
    /// remote-to-remote transfers
    #[arg(
        long = "dual-remote",
        num_args = 2,
        value_names = ["SOURCE", "TARGET"],
        conflicts_with_all = ["target", "local"]
    )]
    dual_remote: Option<Vec<String>>,

    /// Virtual relay: reach each host through a relay so the client is
    /// never a network peer of A or B. Three args (RELAY HOST_A HOST_B)
    /// share one relay; four (RELAY_A HOST_A RELAY_B HOST_B) give each
    /// endpoint its own relay, so A and B never share an observed peer.
    /// Each RELAY may be a multi-hop chain (-J syntax): user@r1,user@r2
    #[arg(
        long = "virtual-relay",
        num_args = 3..=4,
        value_names = ["RELAY", "HOST_A", "HOST_B"],
        conflicts_with_all = ["target", "local", "dual_remote"]
    )]
    virtual_relay: Option<Vec<String>>,

    /// Path to SSH private key
    #[arg(short = 'i', long = "identity")]
    identity_file: Option<String>,

    /// ProxyJump chain (ssh -J syntax): comma-separated [user@]host[:port]
    /// hops to tunnel through. Applies to every target being connected.
    #[arg(short = 'J', long = "jump", value_name = "JUMPS")]
    jump: Option<String>,

    /// Force color mode: auto, truecolor, 256, ansi, none
    #[arg(long = "color", default_value = "auto")]
    color_mode: String,

    /// Benchmark transfer throughput against TARGET instead of opening the
    /// browser: SFTP path vs raw exec stream vs system scp
    #[arg(
        long,
        value_name = "SIZE_MIB",
        num_args = 0..=1,
        default_missing_value = "256",
        require_equals = true,
        requires = "target",
        conflicts_with_all = ["local", "dual_remote"]
    )]
    bench: Option<u64>,
}

/// Parse and validate the two network targets a dual-target mode requires.
fn parse_target_pair(
    pair: &[String],
    identity_file: Option<&String>,
) -> Result<(ConnectionInfo, ConnectionInfo)> {
    let source = ConnectionInfo::parse(&pair[0], identity_file.cloned())
        .with_context(|| format!("Invalid source target: {}", pair[0]))?;
    let target = ConnectionInfo::parse(&pair[1], identity_file.cloned())
        .with_context(|| format!("Invalid destination target: {}", pair[1]))?;
    Ok((source, target))
}

/// Dual-remote mode: the full dual-pane TUI with both panes on remote
/// hosts and direct remote-to-remote transfers between them. Targets may
/// carry ProxyJump chains - that is the `--virtual-relay` path.
async fn run_dual_remote(
    left: ConnectionInfo,
    right: ConnectionInfo,
    theme: Theme,
    keymap: keymap::Keymap,
    icons: &'static ui::icons::IconSet,
) -> Result<()> {
    let left_sftp = connect_target(&left).await?;
    let right_sftp = connect_target(&right).await?;

    let app =
        App::new_dual_remote(left_sftp, &left, right_sftp, &right, theme, keymap, icons).await?;
    run_tui(app).await
}

/// Parse the virtual-relay arguments into the two endpoints, each carrying
/// its relay chain as ProxyJump hops. This is the unification: a virtual
/// relay is just dual-remote whose targets are jumped.
///
/// Each RELAY argument is a `-J`-style chain: one or more comma-separated
/// `[user@]host[:port]` hops, traversed in order.
///
/// - 3 args `RELAY HOST_A HOST_B`: both endpoints jump through `RELAY`.
/// - 4 args `RELAY_A HOST_A RELAY_B HOST_B`: each endpoint via its own
///   relay chain, so A and B share no observed peer and cannot be linked.
fn parse_relay_endpoints(
    args: &[String],
    identity_file: Option<&String>,
) -> Result<(ConnectionInfo, ConnectionInfo)> {
    let parse = |raw: &str, role: &str| {
        ConnectionInfo::parse(raw, identity_file.cloned())
            .with_context(|| format!("Invalid {}: {}", role, raw))
    };
    let parse_chain = |raw: &str, role: &str| -> Result<Vec<ConnectionInfo>> {
        let chain = ConnectionInfo::parse_jump_chain(raw, identity_file.cloned())
            .with_context(|| format!("Invalid {}: {}", role, raw))?;
        // An empty chain would silently degrade to plain dual-remote,
        // defeating the mode's purpose; refuse it instead.
        if chain.is_empty() {
            anyhow::bail!("Empty {}: {:?}", role, raw);
        }
        Ok(chain)
    };

    match args.len() {
        3 => {
            let relay = parse_chain(&args[0], "relay chain")?;
            let host_a = parse(&args[1], "host A")?.with_jumps(relay.clone());
            let host_b = parse(&args[2], "host B")?.with_jumps(relay);
            Ok((host_a, host_b))
        }
        4 => {
            let relay_a = parse_chain(&args[0], "relay chain for host A")?;
            let host_a = parse(&args[1], "host A")?.with_jumps(relay_a);
            let relay_b = parse_chain(&args[2], "relay chain for host B")?;
            let host_b = parse(&args[3], "host B")?.with_jumps(relay_b);
            Ok((host_a, host_b))
        }
        // clap enforces num_args = 3..=4, so this is unreachable in practice.
        n => anyhow::bail!("--virtual-relay expects 3 or 4 arguments, got {}", n),
    }
}

/// Virtual relay mode: each endpoint is reached through its relay (a
/// one-hop jump), so the client is never a network peer of A or B. Data
/// still rounds through the client (it holds both SSH sessions and bridges
/// them), but A and B see only their relay; with two distinct relays the
/// endpoints share no observed peer.
///
/// This is `run_dual_remote` where both targets carry a jump chain - so it
/// runs the same path, tunneling each connection through its relay.
async fn run_virtual_relay(
    host_a: ConnectionInfo,
    host_b: ConnectionInfo,
    theme: Theme,
    keymap: keymap::Keymap,
    icons: &'static ui::icons::IconSet,
) -> Result<()> {
    run_dual_remote(host_a, host_b, theme, keymap, icons).await
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let color_support = match args.color_mode.to_lowercase().as_str() {
        "truecolor" | "rgb" | "24bit" => Some(ColorSupport::TrueColor),
        "256" | "palette" => Some(ColorSupport::Palette256),
        "ansi" | "16" | "basic" => Some(ColorSupport::Ansi),
        "none" | "off" | "no" => Some(ColorSupport::None),
        _ => None,
    };

    let config = config::load();
    for warning in &config.warnings {
        eprintln!("Config warning: {}", warning);
    }

    let mut theme = match color_support {
        Some(cap) => Theme::with_capability(cap),
        None => Theme::auto(),
    };
    theme.apply_overrides(&config.theme);

    let icons = config.icons.unwrap_or_else(ui::icons::detect);

    // A global -J chain prefixes the jumps of every target being connected
    // (you reach the -J hosts first, then any mode-specific relay).
    let outer_jumps = match &args.jump {
        Some(spec) => ConnectionInfo::parse_jump_chain(spec, args.identity_file.clone())?,
        None => Vec::new(),
    };
    let jumped = |conn: ConnectionInfo| -> ConnectionInfo {
        if outer_jumps.is_empty() {
            conn
        } else {
            // -J replaces a ProxyJump chain that came from ssh_config (as
            // ssh does), but prefixes explicit mode relays (virtual-relay
            // chains) so the -J hosts are still reached first.
            let mut chain = outer_jumps.clone();
            if !conn.jumps_from_config {
                chain.extend(conn.jumps.iter().cloned());
            }
            conn.with_jumps(chain)
        }
    };

    // Dual-target modes: validate targets and dispatch before any
    // terminal setup.
    if let Some(pair) = &args.dual_remote {
        let (source, target) = parse_target_pair(pair, args.identity_file.as_ref())?;
        return run_dual_remote(jumped(source), jumped(target), theme, config.keymap, icons).await;
    }
    if let Some(targets) = &args.virtual_relay {
        let (host_a, host_b) = parse_relay_endpoints(targets, args.identity_file.as_ref())?;
        return run_virtual_relay(jumped(host_a), jumped(host_b), theme, config.keymap, icons).await;
    }

    let app = if let Some(dir) = &args.local {
        let right_path = PathBuf::from(dir)
            .canonicalize()
            .with_context(|| format!("Invalid directory: {}", dir))?;
        if !right_path.is_dir() {
            anyhow::bail!("Not a directory: {}", dir);
        }
        App::new_local(std::env::current_dir()?, right_path, theme, config.keymap, icons).await?
    } else {
        let target = args.target.as_deref().expect("clap requires target without --local");
        let conn = jumped(ConnectionInfo::parse(target, args.identity_file.clone())?);

        println!("Connecting to {}...", conn.display_name());

        let sftp = match SftpClientShared::connect(&conn).await {
            Ok(sftp) => sftp,
            Err(e) => {
                eprintln!("Connection failed: {:#}", e);
                return Err(e);
            }
        };

        println!("Connected!");

        if let Some(size_mib) = args.bench {
            return bench::run(&sftp, &conn, size_mib).await;
        }

        App::new_connected(sftp, &conn, theme, config.keymap, icons).await?
    };

    run_tui(app).await
}

/// Terminal lifecycle around the event loop: install the panic hook, enter
/// raw mode + alternate screen, run the app, then restore unconditionally.
async fn run_tui(app: App) -> Result<()> {
    install_panic_hook();

    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = run_app(&mut terminal, app).await;

    stdout().execute(DisableMouseCapture)?;
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;

    if let Err(e) = &result {
        eprintln!("Error: {:#}", e);
    }

    result
}

/// Connect one target (walking its ProxyJump chain, if any), printing
/// progress. Used by the multi-connection modes.
async fn connect_target(conn: &ConnectionInfo) -> Result<SftpClientShared> {
    println!("Connecting to {}...", conn.display_name());
    let sftp = SftpClientShared::connect(conn).await?;
    println!("Connected to {}", conn.host);
    Ok(sftp)
}

/// Restore the terminal before the panic message prints, so a panic
/// mid-TUI (which aborts in release builds) doesn't leave the user's
/// shell in raw mode on the alternate screen.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = stdout().execute(DisableMouseCapture);
        let _ = stdout().execute(LeaveAlternateScreen);
        let _ = disable_raw_mode();
        default_hook(info);
    }));
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    mut app: App,
) -> Result<()> {
    loop {
        app.poll_transfer().await;

        if app.should_quit {
            break;
        }

        terminal.draw(|frame| ui::render(frame, &mut app))?;

        let poll_timeout = if app.is_transferring() {
            std::time::Duration::from_millis(50)
        } else {
            std::time::Duration::from_millis(100)
        };

        if event::poll(poll_timeout)? {
            if let Some(action) = input::handle_event(&mut app, event::read()?) {
                app.dispatch(action).await;
            }
        }
    }

    Ok(())
}
