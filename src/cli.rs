use anyhow::{Context, Result};
use russh_config::HostConfig;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
/// path, otherwise ~/.ssh/config. Preprocessed once per process (Include
/// expansion, Key=value normalization, Match-block stripping), with any
/// warnings printed on first load.
fn ssh_config_contents() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let home = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf());
            let path = match std::env::var("SSH_FILES_SSH_CONFIG") {
                Ok(p) if !p.is_empty() => PathBuf::from(p),
                _ => home.as_ref()?.join(".ssh").join("config"),
            };
            if !path.exists() {
                return None;
            }
            let ssh_dir = home.map(|h| h.join(".ssh")).unwrap_or_default();
            let mut warnings = Vec::new();
            let mut visited = Vec::new();
            let contents =
                preprocess_ssh_config(&path, &ssh_dir, 0, &mut visited, &mut warnings)?;
            for w in warnings {
                eprintln!("Warning: ssh config: {}", w);
            }
            Some(contents)
        })
        .clone()
}

/// The directives whose silent omission would change where or how a
/// connection is made. Anything else unknown is skipped quietly, like
/// every partial ssh_config implementation does.
const CONSEQUENTIAL_UNSUPPORTED: &[&str] = &[
    "proxycommand",
    "identityagent",
    "certificatefile",
    "pkcs11provider",
    "securitykeyprovider",
];

/// OpenSSH file-level semantics that russh-config's parser lacks:
///
/// - `Include` directives are expanded in place (glob patterns, `~`,
///   relative paths resolved against ~/.ssh, depth-capped, cycles
///   skipped), exactly as ssh does for user configs.
/// - `Key=value` / `Key = value` forms are normalized to `Key value`
///   (ssh accepts both spellings).
/// - `Match` blocks are dropped whole, with a warning. They cannot be
///   merely left in: the parser ignores the unknown `Match` line but
///   would then misattribute the block's body to the enclosing Host.
/// - Consequential directives we do not honor produce one warning each.
fn preprocess_ssh_config(
    path: &Path,
    ssh_dir: &Path,
    depth: usize,
    visited: &mut Vec<PathBuf>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if depth > 16 {
        warnings.push(format!("{}: Include nesting too deep, skipped", path.display()));
        return Some(String::new());
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if visited.contains(&canonical) {
        return Some(String::new());
    }
    visited.push(canonical);

    let raw = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            if depth == 0 {
                eprintln!("Warning: could not read {}: {}", path.display(), e);
                return None;
            }
            // Missing or unreadable included files are skipped, as ssh does
            // with unmatched Include patterns.
            return Some(String::new());
        }
    };

    let mut out = String::new();
    let mut in_match_block = false;
    for line in raw.lines() {
        let line = normalize_key_value(line);
        let mut tokens = line.split_whitespace();
        let keyword = tokens.next().unwrap_or("").to_ascii_lowercase();

        if in_match_block {
            if keyword == "host" {
                in_match_block = false;
            } else {
                continue;
            }
        }

        match keyword.as_str() {
            "match" => {
                if !warnings.iter().any(|w| w.contains("'Match'")) {
                    warnings.push("'Match' blocks are not supported; ignored".to_string());
                }
                in_match_block = true;
            }
            "include" => {
                for pattern in tokens {
                    let pattern = pattern.trim_matches('"');
                    let resolved = resolve_include_pattern(pattern, ssh_dir);
                    let Ok(matches) = glob::glob(&resolved) else { continue };
                    for file in matches.flatten() {
                        if let Some(expanded) =
                            preprocess_ssh_config(&file, ssh_dir, depth + 1, visited, warnings)
                        {
                            out.push_str(&expanded);
                        }
                    }
                }
            }
            _ => {
                if CONSEQUENTIAL_UNSUPPORTED.contains(&keyword.as_str())
                    && !warnings.iter().any(|w| w.contains(&format!("'{}'", keyword)))
                {
                    warnings.push(format!("'{}' is not supported; ignored", keyword));
                }
                out.push_str(&line);
                out.push('\n');
            }
        }
    }
    Some(out)
}

/// Resolve an Include pattern the way ssh does for user configs: `~`
/// expands to the home directory, and relative patterns are taken
/// relative to ~/.ssh.
fn resolve_include_pattern(pattern: &str, ssh_dir: &Path) -> String {
    if let Some(rest) = pattern.strip_prefix("~/") {
        if let Some(dirs) = directories::UserDirs::new() {
            return dirs.home_dir().join(rest).to_string_lossy().into_owned();
        }
    }
    if Path::new(pattern).is_absolute() {
        return pattern.to_string();
    }
    ssh_dir.join(pattern).to_string_lossy().into_owned()
}

/// Rewrite `Key=value` (with optional spaces around `=`) as `Key value`.
/// Only an `=` glued to the first token counts: `SetEnv FOO=bar` keeps
/// its `=` because the keyword is already whitespace-delimited.
fn normalize_key_value(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let keyword_end = trimmed
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(trimmed.len());
    let (keyword, rest) = trimmed.split_at(keyword_end);
    if keyword.is_empty() {
        return line.to_string();
    }
    let after = rest.trim_start();
    if let Some(value) = after.strip_prefix('=') {
        return format!("{}{} {}", indent, keyword, value.trim_start());
    }
    line.to_string()
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

    #[test]
    fn key_value_lines_are_normalized() {
        assert_eq!(normalize_key_value("Port=2203"), "Port 2203");
        assert_eq!(normalize_key_value("  Port = 2203"), "  Port 2203");
        assert_eq!(normalize_key_value("Port 2203"), "Port 2203");
        // A whitespace-delimited keyword keeps '=' in its value.
        assert_eq!(normalize_key_value("SetEnv FOO=bar"), "SetEnv FOO=bar");
        assert_eq!(normalize_key_value(""), "");
    }

    /// Include expansion end-to-end: globbed relative patterns resolve
    /// against the ssh dir, Key=value inside included files normalizes,
    /// Match blocks are dropped whole (their bodies must NOT leak into
    /// the enclosing Host), and include cycles terminate.
    #[test]
    fn preprocess_expands_includes_and_strips_match() {
        let dir = tempfile::TempDir::new().unwrap();
        let ssh = dir.path();
        std::fs::create_dir(ssh.join("config.d")).unwrap();
        std::fs::write(
            ssh.join("config"),
            "Include config.d/*.conf\nInclude config\nHost plain\n  Port 22\n",
        )
        .unwrap();
        std::fs::write(
            ssh.join("config.d").join("a.conf"),
            "Host alias\n  HostName=real.example\n  Port = 2222\nMatch host *.prod\n  User evil\nHost other\n  User good\n",
        )
        .unwrap();

        let mut warnings = Vec::new();
        let mut visited = Vec::new();
        let text =
            preprocess_ssh_config(&ssh.join("config"), ssh, 0, &mut visited, &mut warnings)
                .unwrap();

        // The include is textually in place and normalized.
        assert!(text.contains("HostName real.example"), "got:\n{}", text);
        assert!(text.contains("Port 2222"));
        // The Match block vanished entirely; what follows it survived.
        assert!(!text.to_lowercase().contains("match"));
        assert!(!text.contains("evil"));
        assert!(text.contains("User good"));
        // Self-include terminated, base content intact.
        assert!(text.contains("Host plain"));
        assert!(warnings.iter().any(|w| w.contains("'Match'")), "warnings: {:?}", warnings);

        // And the real parser resolves a host out of the expanded text.
        let cfg = russh_config::parse(&text, "alias").unwrap().host_config;
        assert_eq!(cfg.hostname.as_deref(), Some("real.example"));
        assert_eq!(cfg.port, Some(2222));
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
