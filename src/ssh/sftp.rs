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
/// order), then the SSH agent, then default key files, then password
/// (pre-supplied if given, otherwise prompted interactively).
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

    if let Some(pass) = password {
        if password_accepted(session, user, pass).await {
            return Ok(());
        }
    }

    // Key methods exhausted: fall back to interactive password attempts,
    // as ssh does. Connects happen before the TUI takes the terminal, so
    // prompting here is safe.
    for _ in 0..3 {
        let pass = crate::cli::read_password(&format!("{}@{}'s password: ", user, host))?;
        if password_accepted(session, user, &pass).await {
            return Ok(());
        }
        eprintln!("Permission denied, please try again.");
    }

    anyhow::bail!("Authentication failed for {}@{}", user, host)
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
