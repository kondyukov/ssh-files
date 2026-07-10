use anyhow::{Context, Result};
use russh_config::HostConfig;
use std::io::{self, Write};
use std::path::PathBuf;

/// Maximum ProxyJump indirection when following ssh_config entries
/// (alias -> ProxyJump -> alias -> ...); guards against config cycles.
const MAX_JUMP_DEPTH: usize = 8;

/// Per-host lookup into the user's ssh_config; injectable so parsing is
/// testable without touching the real filesystem.
type ConfigLookup<'a> = dyn Fn(&str) -> Option<HostConfig> + 'a;

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub user: String,
    pub host: String,
    pub port: u16,
    pub remote_path: Option<String>,
    /// Identity files to try in order: the explicit -i key first, then any
    /// IdentityFile entries from ssh_config.
    pub identity_files: Vec<String>,
    pub password: Option<String>,
    /// ProxyJump chain, in connection order (first is reached first, last
    /// tunnels to this host). Empty for a direct connection. Each hop is
    /// itself a `ConnectionInfo` with its own empty chain.
    pub jumps: Vec<ConnectionInfo>,
    /// Whether `jumps` came from ssh_config's ProxyJump rather than the
    /// command line: -J replaces a config chain (as ssh does) but composes
    /// with explicit mode relays.
    pub jumps_from_config: bool,
}

impl ConnectionInfo {
    /// Parse SSH-style connection string: [user@]host[:port][:path].
    ///
    /// The host is looked up in ~/.ssh/config; `HostName`, `User`, `Port`,
    /// `IdentityFile`, and `ProxyJump` fill in anything not given
    /// explicitly (explicit values always win, as with ssh).
    pub fn parse(target: &str, identity_file: Option<String>) -> Result<Self> {
        Self::parse_with(target, identity_file, &default_lookup(), 0)
    }

    fn parse_with(
        target: &str,
        identity_file: Option<String>,
        lookup: &ConfigLookup,
        depth: usize,
    ) -> Result<Self> {
        let mut user: Option<String> = None;
        let mut host: String;
        let mut port: u16 = 22;
        let mut explicit_port = false;
        let mut remote_path: Option<String> = None;

        let after_user = if let Some(at_pos) = target.find('@') {
            user = Some(target[..at_pos].to_string());
            &target[at_pos + 1..]
        } else {
            target
        };

        let parts: Vec<&str> = after_user.splitn(3, ':').collect();

        match parts.len() {
            1 => {
                host = parts[0].to_string();
            }
            2 => {
                host = parts[0].to_string();
                if let Ok(p) = parts[1].parse::<u16>() {
                    port = p;
                    explicit_port = true;
                } else {
                    remote_path = Some(parts[1].to_string());
                }
            }
            3 => {
                host = parts[0].to_string();
                port = parts[1].parse().context("Invalid port number")?;
                explicit_port = true;
                remote_path = Some(parts[2].to_string());
            }
            _ => unreachable!(),
        }

        let mut identity_files: Vec<String> = identity_file.into_iter().collect();
        let mut jumps = Vec::new();
        let mut jumps_from_config = false;

        // The config is queried under the name as typed (the alias);
        // HostName then replaces it for the actual connection, so host-key
        // verification happens under the real name, matching ssh.
        if let Some(cfg) = lookup(&host) {
            if user.is_none() {
                user = cfg.user.clone();
            }
            if !explicit_port {
                if let Some(p) = cfg.port {
                    port = p;
                }
            }
            if let Some(files) = &cfg.identity_file {
                identity_files.extend(files.iter().map(|p| p.to_string_lossy().into_owned()));
            }
            if let Some(proxy_jump) = &cfg.proxy_jump {
                if depth >= MAX_JUMP_DEPTH {
                    anyhow::bail!(
                        "ProxyJump chain in ssh config exceeds {} levels at host {} (cycle?)",
                        MAX_JUMP_DEPTH,
                        host
                    );
                }
                jumps = Self::parse_jump_chain_with(proxy_jump, None, lookup, depth + 1)?;
                jumps_from_config = true;
            }
            if let Some(hostname) = &cfg.hostname {
                host = hostname.clone();
            }
        }

        let user = match user {
            Some(u) => u,
            None => prompt_username(&host)?,
        };

        Ok(Self {
            user,
            host,
            port,
            remote_path,
            identity_files,
            password: None,
            jumps,
            jumps_from_config,
        })
    }

    /// Parse a ProxyJump chain (`ssh -J` syntax): comma-separated
    /// `[user@]host[:port]` hops in connection order. Each hop reuses the
    /// target parser, so ssh_config resolution and username defaulting
    /// apply per hop. Jump hosts carry no remote path.
    pub fn parse_jump_chain(
        spec: &str,
        identity_file: Option<String>,
    ) -> Result<Vec<ConnectionInfo>> {
        Self::parse_jump_chain_with(spec, identity_file, &default_lookup(), 0)
    }

    fn parse_jump_chain_with(
        spec: &str,
        identity_file: Option<String>,
        lookup: &ConfigLookup,
        depth: usize,
    ) -> Result<Vec<ConnectionInfo>> {
        spec.split(',')
            .map(str::trim)
            .filter(|hop| !hop.is_empty())
            .map(|hop| {
                Self::parse_with(hop, identity_file.clone(), lookup, depth)
                    .with_context(|| format!("Invalid jump host: {}", hop))
            })
            .collect()
    }

    pub fn with_jumps(mut self, jumps: Vec<ConnectionInfo>) -> Self {
        self.jumps = jumps;
        self.jumps_from_config = false;
        self
    }

    /// "user@host:port" for status and error messages, prefixed with the
    /// jump chain when present (e.g. "via bastion -> user@host:22").
    pub fn display_name(&self) -> String {
        let endpoint = format!("{}@{}:{}", self.user, self.host, self.port);
        if self.jumps.is_empty() {
            endpoint
        } else {
            let chain: Vec<String> = self.jumps.iter().map(|j| j.host.clone()).collect();
            format!("via {} -> {}", chain.join(","), endpoint)
        }
    }
}

/// Lookup backed by the user's real ssh_config. The file is read once per
/// parse; queries against it are pure. A missing file is normal (lookup
/// yields nothing); an unreadable or unparseable one warns once and is
/// then ignored - config trouble must never block a connection.
fn default_lookup() -> impl Fn(&str) -> Option<HostConfig> {
    let contents = ssh_config_contents();
    move |host: &str| {
        let contents = contents.as_ref()?;
        match russh_config::parse(contents, host) {
            Ok(config) => Some(config.host_config),
            Err(e) => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| eprintln!("Warning: ignoring ssh config: {}", e));
                None
            }
        }
    }
}

/// Contents of the ssh config file: $SSH_FILES_SSH_CONFIG overrides the
/// path, otherwise ~/.ssh/config.
fn ssh_config_contents() -> Option<String> {
    let path = match std::env::var("SSH_FILES_SSH_CONFIG") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => directories::UserDirs::new()?
            .home_dir()
            .join(".ssh")
            .join("config"),
    };
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(contents) => Some(contents),
        Err(e) => {
            eprintln!("Warning: could not read {}: {}", path.display(), e);
            None
        }
    }
}

fn prompt_username(host: &str) -> Result<String> {
    let default = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();

    let prompt = if default.is_empty() {
        format!("Username for {}: ", host)
    } else {
        format!("Username for {} [{}]: ", host, default)
    };

    print!("{}", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    if input.is_empty() && !default.is_empty() {
        Ok(default)
    } else if input.is_empty() {
        anyhow::bail!("Username required")
    } else {
        Ok(input.to_string())
    }
}

#[cfg(unix)]
pub(crate) fn read_password(prompt: &str) -> Result<String> {
    use std::os::unix::io::AsRawFd;
    
    print!("{}", prompt);
    io::stdout().flush()?;
    
    let stdin_fd = io::stdin().as_raw_fd();
    
    let mut termios = unsafe {
        let mut t = std::mem::zeroed();
        if libc::tcgetattr(stdin_fd, &mut t) != 0 {
            return read_password_simple();
        }
        t
    };
    
    let old_termios = termios;
    termios.c_lflag &= !libc::ECHO;
    
    unsafe {
        libc::tcsetattr(stdin_fd, libc::TCSANOW, &termios);
    }
    
    let mut password = String::new();
    let result = io::stdin().read_line(&mut password);
    
    unsafe {
        libc::tcsetattr(stdin_fd, libc::TCSANOW, &old_termios);
    }
    
    println!();
    
    result?;
    Ok(password.trim().to_string())
}

#[cfg(windows)]
pub(crate) fn read_password(prompt: &str) -> Result<String> {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, SetConsoleMode, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
    };
    use std::os::windows::io::AsRawHandle;
    
    print!("{}", prompt);
    io::stdout().flush()?;
    
    let handle = io::stdin().as_raw_handle() as *mut std::ffi::c_void;
    
    let mut mode: u32 = 0;
    unsafe {
        if GetConsoleMode(handle, &mut mode) == 0 {
            return read_password_simple();
        }
    }
    
    let new_mode = (mode & !ENABLE_ECHO_INPUT) | ENABLE_LINE_INPUT;
    
    unsafe {
        SetConsoleMode(handle, new_mode);
    }
    
    let mut password = String::new();
    let result = io::stdin().read_line(&mut password);
    
    unsafe {
        SetConsoleMode(handle, mode);
    }
    
    println!();
    
    result?;
    Ok(password.trim().to_string())
}

fn read_password_simple() -> Result<String> {
    let mut password = String::new();
    io::stdin().read_line(&mut password)?;
    Ok(password.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No ssh_config at all: tests must never read the developer's real
    /// ~/.ssh/config or they become machine-dependent.
    fn no_config(_host: &str) -> Option<HostConfig> {
        None
    }

    /// Lookup backed by a literal config string (exercises the real
    /// russh-config parser).
    fn lookup_from(config: &str) -> impl Fn(&str) -> Option<HostConfig> + '_ {
        move |host| russh_config::parse(config, host).ok().map(|c| c.host_config)
    }

    fn parse(target: &str, identity: Option<String>, lookup: &ConfigLookup) -> Result<ConnectionInfo> {
        ConnectionInfo::parse_with(target, identity, lookup, 0)
    }

    #[test]
    fn target_with_explicit_user_does_not_prompt() {
        let c = parse("alice@host:2222:/data", None, &no_config).unwrap();
        assert_eq!(c.user, "alice");
        assert_eq!(c.host, "host");
        assert_eq!(c.port, 2222);
        assert_eq!(c.remote_path.as_deref(), Some("/data"));
        assert!(c.jumps.is_empty());
        assert!(c.identity_files.is_empty());
    }

    #[test]
    fn jump_chain_parses_in_order() {
        let jumps = ConnectionInfo::parse_jump_chain_with(
            "u1@bastion1,u2@bastion2:2022",
            None,
            &no_config,
            0,
        )
        .unwrap();
        assert_eq!(jumps.len(), 2);
        assert_eq!(jumps[0].host, "bastion1");
        assert_eq!(jumps[0].port, 22);
        assert_eq!(jumps[1].host, "bastion2");
        assert_eq!(jumps[1].port, 2022);
        // Each hop is itself jump-free (flat chain).
        assert!(jumps.iter().all(|j| j.jumps.is_empty()));
    }

    #[test]
    fn empty_and_whitespace_chain_yields_no_jumps() {
        let parse_chain = |spec: &str| {
            ConnectionInfo::parse_jump_chain_with(spec, None, &no_config, 0).unwrap()
        };
        assert!(parse_chain("").is_empty());
        assert!(parse_chain(" , ").is_empty());
    }

    #[test]
    fn display_name_shows_jump_chain() {
        let target = parse("alice@host", None, &no_config)
            .unwrap()
            .with_jumps(
                ConnectionInfo::parse_jump_chain_with("u@bastion", None, &no_config, 0).unwrap(),
            );
        assert_eq!(target.display_name(), "via bastion -> alice@host:22");
    }

    #[test]
    fn config_fills_hostname_user_port_identity() {
        let config = "Host hpc\n  HostName real.example.com\n  User bob\n  Port 2200\n  IdentityFile /keys/hpc\n";
        let c = parse("hpc", None, &lookup_from(config)).unwrap();
        assert_eq!(c.host, "real.example.com");
        assert_eq!(c.user, "bob");
        assert_eq!(c.port, 2200);
        assert_eq!(c.identity_files, vec!["/keys/hpc".to_string()]);
        assert!(c.jumps.is_empty());
    }

    #[test]
    fn explicit_values_beat_config() {
        let config = "Host hpc\n  HostName real.example.com\n  User bob\n  Port 2200\n  IdentityFile /keys/hpc\n";
        let c = parse("alice@hpc:2222", Some("/cli/key".into()), &lookup_from(config)).unwrap();
        assert_eq!(c.user, "alice");
        assert_eq!(c.port, 2222);
        // HostName still applies - only the alias was typed.
        assert_eq!(c.host, "real.example.com");
        // Explicit -i comes first; config identities follow.
        assert_eq!(c.identity_files, vec!["/cli/key".to_string(), "/keys/hpc".to_string()]);
    }

    #[test]
    fn unknown_host_is_untouched() {
        let config = "Host hpc\n  User bob\n";
        let c = parse("alice@elsewhere", None, &lookup_from(config)).unwrap();
        assert_eq!(c.host, "elsewhere");
        assert_eq!(c.port, 22);
        assert!(c.identity_files.is_empty());
    }

    #[test]
    fn config_proxy_jump_resolves_recursively() {
        let config = "Host inner\n  User w\n  ProxyJump bastion\nHost bastion\n  HostName b.example.com\n  User jumpuser\n";
        let c = parse("inner", None, &lookup_from(config)).unwrap();
        assert!(c.jumps_from_config);
        assert_eq!(c.jumps.len(), 1);
        assert_eq!(c.jumps[0].host, "b.example.com");
        assert_eq!(c.jumps[0].user, "jumpuser");
    }

    #[test]
    fn proxy_jump_cycle_errors_instead_of_hanging() {
        let config = "Host a\n  User x\n  ProxyJump b\nHost b\n  User y\n  ProxyJump a\n";
        let err = parse("a", None, &lookup_from(config)).unwrap_err();
        assert!(format!("{:#}", err).contains("levels"), "got: {:#}", err);
    }

    #[test]
    fn with_jumps_marks_chain_explicit() {
        let config = "Host inner\n  User w\n  ProxyJump bastion\nHost bastion\n  User j\n";
        let lookup = lookup_from(config);
        let relay = parse("r@relay", None, &lookup).unwrap();
        let c = parse("inner", None, &lookup).unwrap().with_jumps(vec![relay]);
        assert!(!c.jumps_from_config);
        assert_eq!(c.jumps[0].host, "relay");
    }
}
