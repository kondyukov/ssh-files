use anyhow::{Context, Result};
use russh::client;
use russh::keys::{self, HashAlg, PrivateKeyWithHashAlg};
use russh_sftp::client::SftpSession;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use crate::cli::ConnectionInfo;
use crate::source::FileInfo;

struct ClientHandler {
    host: String,
    port: u16,
}

impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        // @revoked markers first: russh's checker skips marker lines, so an
        // explicitly banned key would otherwise degrade to a TOFU prompt.
        // Errors reading the file also refuse the connection (fail closed).
        if super::revoked::key_is_revoked(&self.host, self.port, server_public_key)? {
            eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
            eprintln!("@       WARNING: REVOKED HOST KEY DETECTED!               @");
            eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
            eprintln!(
                "The {} host key presented by '{}' is marked @revoked in known_hosts",
                server_public_key.algorithm(),
                self.host
            );
            eprintln!("and must never be accepted.");
            eprintln!(
                "Revoked key fingerprint: {}",
                server_public_key.fingerprint(HashAlg::Sha256)
            );
            anyhow::bail!("Host key verification failed: revoked key.")
        }

        match keys::check_known_hosts(&self.host, self.port, server_public_key) {
            // Key matches the known_hosts record.
            Ok(true) => Ok(true),
            // Host not yet known: trust on first use, with confirmation.
            Ok(false) => self.confirm_unknown_key(server_public_key),
            // Key differs from the known_hosts record: possible MITM, hard fail.
            Err(keys::Error::KeyChanged { line }) => {
                eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
                eprintln!("@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @");
                eprintln!("@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@");
                eprintln!("Someone could be eavesdropping on you right now (man-in-the-middle attack)!");
                eprintln!(
                    "The {} host key for '{}' does not match the key recorded in known_hosts (line {}).",
                    server_public_key.algorithm(),
                    self.host,
                    line
                );
                // Fingerprint's Display already carries the "SHA256:" prefix.
                eprintln!(
                    "Offending key fingerprint: {}",
                    server_public_key.fingerprint(HashAlg::Sha256)
                );
                eprintln!("If the change is expected, remove the old key from known_hosts and reconnect.");
                anyhow::bail!("Host key verification failed.")
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl ClientHandler {
    fn confirm_unknown_key(&self, key: &keys::PublicKey) -> Result<bool> {
        println!(
            "The authenticity of host '{}' (port {}) can't be established.",
            self.host, self.port
        );
        println!(
            "{} key fingerprint is {}.",
            key.algorithm(),
            key.fingerprint(HashAlg::Sha256)
        );
        // A CA-trusted host presents as unknown here because host
        // certificates are unsupported (blocked upstream in russh — see
        // revoked::cert_authority_matches for the seams). Say so, rather
        // than silently ignoring the user's CA configuration.
        if super::revoked::cert_authority_matches(&self.host, self.port) {
            println!(
                "Note: a @cert-authority entry in known_hosts matches this host, but this"
            );
            println!(
                "client does not support host certificates; verify the fingerprint above"
            );
            println!("against your CA's issuance records.");
        }
        print!("Are you sure you want to continue connecting (yes/no)? ");
        io::stdout().flush()?;

        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;

        match answer.trim().to_ascii_lowercase().as_str() {
            "yes" | "y" => {
                match keys::known_hosts::learn_known_hosts(&self.host, self.port, key) {
                    Ok(()) => println!(
                        "Warning: Permanently added '{}' to the list of known hosts.",
                        self.host
                    ),
                    Err(e) => eprintln!("Warning: could not record host key: {}", e),
                }
                Ok(true)
            }
            _ => anyhow::bail!("Host key verification failed."),
        }
    }
}

/// Cheap-to-clone handle for opening exec channels on the SSH connection.
/// This is what the raw streaming transfer path rides on.
#[derive(Clone)]
pub struct ExecHandle {
    handle: Arc<client::Handle<ClientHandler>>,
}

impl ExecHandle {
    /// Open a session channel and exec `command` on the remote, returning
    /// the channel for streaming I/O.
    pub async fn open_exec(&self, command: &str) -> Result<russh::Channel<client::Msg>> {
        let channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;
        Ok(channel)
    }
}

/// Try public-key auth with a key file, prompting for the passphrase if
/// the key is encrypted (empty input skips the key), as ssh does.
async fn try_key_file(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    path: &Path,
) -> bool {
    let key = match keys::load_secret_key(path, None) {
        Ok(key) => key,
        Err(keys::Error::KeyIsEncrypted) => {
            let prompt = format!(
                "Enter passphrase for key '{}' (empty to skip): ",
                path.display()
            );
            let Ok(passphrase) = crate::cli::read_password(&prompt) else {
                return false;
            };
            if passphrase.is_empty() {
                return false;
            }
            match keys::load_secret_key(path, Some(&passphrase)) {
                Ok(key) => key,
                Err(e) => {
                    eprintln!("Could not decrypt {}: {}", path.display(), e);
                    return false;
                }
            }
        }
        Err(_) => return false,
    };

    // RSA keys need the negotiated rsa-sha2 variant; other algorithms
    // take no hash parameter.
    let hash_alg = match rsa_hash(session, key.algorithm()).await {
        Ok(h) => h,
        Err(_) => return false,
    };
    session
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg))
        .await
        .map(|r| r.success())
        .unwrap_or(false)
}

/// The rsa-sha2 hash to sign with, negotiated with the server - `None`
/// for non-RSA keys and for servers that only speak legacy ssh-rsa.
async fn rsa_hash(
    session: &mut client::Handle<ClientHandler>,
    algorithm: keys::Algorithm,
) -> Result<Option<HashAlg>> {
    if !matches!(algorithm, keys::Algorithm::Rsa { .. }) {
        return Ok(None);
    }
    Ok(session.best_supported_rsa_hash().await?.flatten())
}

/// Try every identity the SSH agent holds, as ssh does by default. The
/// agent only ever signs; keys never leave it.
#[cfg(unix)]
async fn try_agent(session: &mut client::Handle<ClientHandler>, user: &str) -> bool {
    use russh::keys::agent::client::AgentClient;

    let Ok(mut agent) = AgentClient::connect_env().await else {
        return false;
    };
    let Ok(identities) = agent.request_identities().await else {
        return false;
    };

    for identity in identities {
        // Certificate identities would need certificate auth plumbing;
        // plain keys cover the standard agent setup.
        let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let Ok(hash_alg) = rsa_hash(session, key.algorithm()).await else {
            return false;
        };
        let result = session
            .authenticate_publickey_with(user, key, hash_alg, &mut agent)
            .await;
        if result.map(|r| r.success()).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// The agent protocol transport in russh-keys is Unix-socket based; on
/// other platforms key files and passwords remain available.
#[cfg(not(unix))]
async fn try_agent(_session: &mut client::Handle<ClientHandler>, _user: &str) -> bool {
    false
}

/// Session config shared by every connection, direct or tunneled. The big
/// window matters for jump hops too: the tunnel to the next hop rides a
/// single channel on the hop's session, so a default-sized hop window
/// would throttle everything flowing through it.
fn session_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        // 32 MiB receive window so downloads are not RTT-bound by the
        // default ~2 MiB channel window on high-latency links. Upload
        // throughput is governed by the window the *server* advertises
        // and is unaffected by this.
        window_size: 32 * 1024 * 1024,
        ..Default::default()
    })
}

/// Run the authentication ladder on a freshly connected session, mirroring
/// ssh: explicit keys (the -i key, then ssh_config IdentityFile entries, in
/// order), then the SSH agent, then default key files, then
/// keyboard-interactive (the PAM reality on most Linux servers), then
/// password (pre-supplied if given, otherwise prompted interactively).
/// Interactive secret prompts share one budget of three attempts across
/// keyboard-interactive and password, like ssh's NumberOfPasswordPrompts.
async fn authenticate(
    session: &mut client::Handle<ClientHandler>,
    host: &str,
    user: &str,
    key_paths: &[String],
    password: Option<&str>,
) -> Result<()> {
    for path in key_paths {
        if try_key_file(session, user, Path::new(path)).await {
            return Ok(());
        }
    }

    if try_agent(session, user).await {
        return Ok(());
    }

    let home = directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_default();

    for key_name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
        let key_path = home.join(".ssh").join(key_name);
        if key_path.exists() && try_key_file(session, user, &key_path).await {
            return Ok(());
        }
    }

    let mut prompts_left: u32 = 3;

    if try_keyboard_interactive(session, user, password, &mut prompts_left).await? {
        return Ok(());
    }

    if let Some(pass) = password {
        if password_accepted(session, user, pass).await {
            return Ok(());
        }
    }

    // Key methods exhausted: fall back to interactive password attempts,
    // as ssh does. Connects happen before the TUI takes the terminal, so
    // prompting here is safe.
    while prompts_left > 0 {
        prompts_left -= 1;
        let pass = crate::cli::read_password(&format!("{}@{}'s password: ", user, host))?;
        if password_accepted(session, user, &pass).await {
            return Ok(());
        }
        eprintln!("Permission denied, please try again.");
    }

    anyhow::bail!("Authentication failed for {}@{}", user, host)
}

/// Keyboard-interactive auth: relay the server's prompt conversation to
/// the terminal, as ssh does. PAM-backed servers commonly advertise only
/// this method, with password auth disabled.
///
/// A server that does not support the method fails the request without
/// ever sending prompts - that case falls through to password auth
/// immediately and consumes no attempt budget. A pre-supplied password
/// answers the first hidden prompt once (the sshpass convention), so
/// scripted connects work against keyboard-interactive-only servers.
async fn try_keyboard_interactive(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    password: Option<&str>,
    prompts_left: &mut u32,
) -> Result<bool> {
    use russh::client::KeyboardInteractiveAuthResponse as Kbd;

    let mut presupplied = password;

    // The rounds are capped independently of the prompt budget: a server
    // that only ever sends echoed prompts would otherwise loop forever.
    'round: for _ in 0..3 {
        if *prompts_left == 0 {
            break;
        }
        let mut response = session
            .authenticate_keyboard_interactive_start(user, None)
            .await?;
        let mut prompted = false;

        loop {
            match response {
                Kbd::Success => return Ok(true),
                Kbd::Failure { .. } => {
                    if !prompted {
                        // Method rejected outright: not supported here.
                        return Ok(false);
                    }
                    eprintln!("Permission denied, please try again.");
                    continue 'round;
                }
                Kbd::InfoRequest { name, instructions, prompts } => {
                    if !name.trim().is_empty() {
                        println!("{}", name.trim());
                    }
                    if !instructions.trim().is_empty() {
                        println!("{}", instructions.trim());
                    }

                    let mut answers = Vec::with_capacity(prompts.len());
                    for p in &prompts {
                        prompted = true;
                        if p.echo {
                            answers.push(read_line_echoed(&p.prompt)?);
                        } else if let Some(pass) = presupplied.take() {
                            answers.push(pass.to_string());
                        } else {
                            if *prompts_left == 0 {
                                return Ok(false);
                            }
                            *prompts_left -= 1;
                            answers.push(crate::cli::read_password(&p.prompt)?);
                        }
                    }

                    response = session
                        .authenticate_keyboard_interactive_respond(answers)
                        .await?;
                }
            }
        }
    }
    Ok(false)
}

/// Read one visible (echoed) line for a keyboard-interactive prompt.
fn read_line_echoed(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim_end_matches(['\r', '\n']).to_string())
}

async fn password_accepted(
    session: &mut client::Handle<ClientHandler>,
    user: &str,
    pass: &str,
) -> bool {
    session
        .authenticate_password(user, pass)
        .await
        .map(|r| r.success())
        .unwrap_or(false)
}

/// Open a session over a direct TCP connection to `host:port`.
async fn open_direct(host: &str, port: u16) -> Result<client::Handle<ClientHandler>> {
    let handler = ClientHandler {
        host: host.to_string(),
        port,
    };
    client::connect(session_config(), (host, port), handler)
        .await
        .with_context(|| format!("Failed to connect to {}:{}", host, port))
}

/// Open a session to `host:port` tunneled through an already-established
/// `parent` session (ProxyJump): the parent opens a direct-tcpip channel
/// to the destination, and a full SSH handshake runs over that channel.
/// The handler carries the *destination's* name, so host-key verification
/// happens per hop under each hop's own known_hosts identity.
async fn open_via(
    parent: &client::Handle<ClientHandler>,
    host: &str,
    port: u16,
) -> Result<client::Handle<ClientHandler>> {
    let channel = parent
        .channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0)
        .await
        .with_context(|| format!("Jump host could not reach {}:{}", host, port))?;
    let handler = ClientHandler {
        host: host.to_string(),
        port,
    };
    client::connect_stream(session_config(), channel.into_stream(), handler)
        .await
        .with_context(|| format!("Failed to connect to {}:{} through jump host", host, port))
}

/// Connect and authenticate one endpoint: direct if `parent` is None,
/// tunneled through `parent` otherwise.
async fn open_authenticated(
    parent: Option<&client::Handle<ClientHandler>>,
    host: &str,
    port: u16,
    user: &str,
    key_paths: &[String],
    password: Option<&str>,
) -> Result<client::Handle<ClientHandler>> {
    let mut session = match parent {
        None => open_direct(host, port).await?,
        Some(p) => open_via(p, host, port).await?,
    };
    authenticate(&mut session, host, user, key_paths, password).await?;
    Ok(session)
}

pub struct SftpClientShared {
    handle: Arc<client::Handle<ClientHandler>>,
    sftp: Arc<SftpSession>,
    /// Jump-hop sessions, outermost first. Never used after connect, but
    /// dropping a hop's handle tears down its session - and with it the
    /// tunnel every later hop and the target ride on - so they must live
    /// exactly as long as the target session. Empty for direct connections.
    _hops: Vec<client::Handle<ClientHandler>>,
}

impl SftpClientShared {
    pub async fn connect(conn: &ConnectionInfo) -> Result<Self> {
        // Walk the ProxyJump chain in connection order: each hop is
        // reached through the previous one and fully authenticated before
        // it tunnels onward. The final target then rides the last hop.
        let mut hops: Vec<client::Handle<ClientHandler>> = Vec::new();
        for hop in &conn.jumps {
            if !hops.is_empty() {
                println!("Tunneling to jump host {}...", hop.host);
            }
            let session = open_authenticated(
                hops.last(),
                &hop.host,
                hop.port,
                &hop.user,
                &hop.identity_files,
                hop.password.as_deref(),
            )
            .await
            .with_context(|| format!("At jump host {}", hop.display_name()))?;
            hops.push(session);
        }

        let session = open_authenticated(
            hops.last(),
            &conn.host,
            conn.port,
            &conn.user,
            &conn.identity_files,
            conn.password.as_deref(),
        )
        .await?;

        let channel = session.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;

        let sftp = SftpSession::new(channel.into_stream()).await?;

        Ok(Self {
            handle: Arc::new(session),
            sftp: Arc::new(sftp),
            _hops: hops,
        })
    }

    pub fn sftp(&self) -> Arc<SftpSession> {
        Arc::clone(&self.sftp)
    }

    pub fn exec_handle(&self) -> ExecHandle {
        ExecHandle {
            handle: Arc::clone(&self.handle),
        }
    }

    /// Open a session channel and exec `command` on the remote, returning
    /// the channel for streaming I/O.
    pub async fn open_exec(&self, command: &str) -> Result<russh::Channel<client::Msg>> {
        self.exec_handle().open_exec(command).await
    }

    /// Initial listing used while resolving the starting directory. Hidden
    /// entries are excluded, matching the default browse view; subsequent
    /// listings go through `RemoteSource::list_dir`.
    pub async fn list_dir(&self, path: &str) -> Result<Vec<FileInfo>> {
        let dir = self.sftp.read_dir(path).await?;

        let mut entries: Vec<FileInfo> = dir.into_iter()
            .filter_map(|entry| {
                let name = entry.file_name();
                if name.starts_with('.') {
                    return None;
                }
                let is_dir = entry.file_type().is_dir();
                let size = entry.metadata().size.unwrap_or(0);
                Some(FileInfo { name, is_dir, size })
            })
            .collect();
        
        entries.sort_by(|a, b| {
            match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
        });
        
        Ok(entries)
    }
}
