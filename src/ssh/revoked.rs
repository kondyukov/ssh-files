//! known_hosts marker (`@revoked`, `@cert-authority`) support.
//!
//! russh's checker ignores marker lines entirely: it reads the first
//! token of every line as the host pattern, so a marker never matches a
//! hostname and the line is skipped. For `@revoked` that silently
//! *degrades* a banned key to an unknown one — the user gets a fresh
//! TOFU prompt for a key they explicitly banned. OpenSSH refuses such a
//! key outright (`sshd(8)` known_hosts format: "the key ... is revoked
//! and must never be accepted"); this module restores that behavior.
//! `@cert-authority` lines cannot be honored (see
//! [`cert_authority_matches`]) but are detected so the TOFU prompt can
//! say so instead of silently ignoring the user's CA configuration.
//!
//! Matching follows OpenSSH `match_pattern_list` semantics: patterns are
//! comma-separated, `*`/`?` glob, a leading `!` negates (and a negated
//! match vetoes the whole list), non-22 ports are written `[host]:port`,
//! and hashed entries (`HashKnownHosts`, `|1|salt|hash`) are HMAC-SHA1
//! verified.

use anyhow::{Context, Result};
use data_encoding::BASE64_MIME;
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::{self, PublicKey};
use sha1::Sha1;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// The spelling ssh-keyscan/OpenSSH store: bare host for the default
/// port, `[host]:port` otherwise. Hostnames match case-insensitively.
fn host_port(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_ascii_lowercase()
    } else {
        format!("[{}]:{}", host.to_ascii_lowercase(), port)
    }
}

fn default_known_hosts() -> Option<PathBuf> {
    directories::UserDirs::new().map(|dirs| dirs.home_dir().join(".ssh").join("known_hosts"))
}

/// Check the user's `~/.ssh/known_hosts` for an `@revoked` entry matching
/// this host and key. Missing file means nothing is revoked; a read error
/// propagates (fail closed — the caller refuses the connection).
pub fn key_is_revoked(host: &str, port: u16, key: &PublicKey) -> Result<bool> {
    let Some(path) = default_known_hosts() else {
        return Ok(false);
    };
    key_is_revoked_in(&path, host, port, key)
}

/// Whether any `@cert-authority` entry in the user's known_hosts matches
/// this host. Advisory only, so best-effort: any problem reads as "no".
///
/// Actually *honoring* the CA line is blocked upstream in russh, not
/// here. For whoever picks this up, the three seams in russh 0.62 are:
/// `negotiation.rs` (`Preferred::DEFAULT.key` never offers the
/// `*-cert-v01@openssh.com` algorithms, so servers never send their
/// certificate), `client/kex.rs` (the host-key blob is parsed as a plain
/// `PublicKey`; a certificate blob would not parse), and
/// `client::Handler::check_server_key` (receives `ssh_key::PublicKey`
/// only — no way to hand a certificate through). Until all three change
/// upstream, the honest move is to tell the user at the TOFU prompt that
/// their CA configuration exists but cannot be used.
pub fn cert_authority_matches(host: &str, port: u16) -> bool {
    let Some(path) = default_known_hosts() else {
        return false;
    };
    cert_authority_matches_in(&path, host, port)
}

fn cert_authority_matches_in(path: &Path, host: &str, port: u16) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let host_port = host_port(host, port);
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { return false };
        let mut tokens = line.split_whitespace();
        if tokens.next() != Some("@cert-authority") {
            continue;
        }
        if let Some(patterns) = tokens.next() {
            if patterns_match(&host_port, patterns) {
                return true;
            }
        }
    }
    false
}

/// The same check against an explicit file (separated for tests).
fn key_is_revoked_in(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<bool> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(false),
    };

    let host_port = host_port(host, port);
    for line in BufReader::new(file).lines() {
        let line = line.context("reading known_hosts")?;
        let mut tokens = line.split_whitespace();
        if tokens.next() != Some("@revoked") {
            continue;
        }
        let (Some(patterns), Some(_keytype), Some(key64)) =
            (tokens.next(), tokens.next(), tokens.next())
        else {
            continue;
        };
        if !patterns_match(&host_port, patterns) {
            continue;
        }
        // An unparseable key on a marker line cannot be compared; ignore
        // the line rather than fail every connection over one bad entry.
        let Ok(recorded) = keys::parse_public_key_base64(key64) else {
            continue;
        };
        if recorded.key_data() == key.key_data() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// OpenSSH `match_pattern_list`: any positive pattern must match, and a
/// matching negated (`!`) pattern vetoes the entire list.
fn patterns_match(host_port: &str, patterns: &str) -> bool {
    let mut matched = false;
    for pattern in patterns.split(',') {
        if let Some(negated) = pattern.strip_prefix('!') {
            if single_pattern_matches(host_port, negated) {
                return false;
            }
        } else if single_pattern_matches(host_port, pattern) {
            matched = true;
        }
    }
    matched
}

fn single_pattern_matches(host_port: &str, pattern: &str) -> bool {
    if pattern.starts_with("|1|") {
        return hashed_matches(host_port, pattern);
    }
    glob_match(pattern.to_ascii_lowercase().as_bytes(), host_port.as_bytes())
}

/// `HashKnownHosts` entries: `|1|base64(salt)|base64(hmac-sha1(salt, host))`.
fn hashed_matches(host_port: &str, pattern: &str) -> bool {
    let mut parts = pattern.split('|').skip(2);
    let Some(Ok(salt)) = parts.next().map(|p| BASE64_MIME.decode(p.as_bytes())) else {
        return false;
    };
    let Some(Ok(hash)) = parts.next().map(|p| BASE64_MIME.decode(p.as_bytes())) else {
        return false;
    };
    let Ok(hmac) = Hmac::<Sha1>::new_from_slice(&salt) else {
        return false;
    };
    hmac.chain_update(host_port).verify_slice(&hash).is_ok()
}

/// Minimal `*`/`?` glob, the two metacharacters OpenSSH patterns define.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.split_first(), text.split_first()) {
        (None, None) => true,
        (Some((b'*', rest)), _) => {
            glob_match(rest, text)
                || !text.is_empty() && glob_match(pattern, &text[1..])
        }
        (Some((b'?', p_rest)), Some((_, t_rest))) => glob_match(p_rest, t_rest),
        (Some((p, p_rest)), Some((t, t_rest))) => p == t && glob_match(p_rest, t_rest),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Throwaway ed25519 keys generated for these tests; never used anywhere.
    const KEY_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIM+DvcwrFD7HjkUN3ceXvicmABNy2ryMXN8WBohqUplc";
    const KEY_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJTrdL9eBKz+lc8ofj4iEA15GuF+hUD40EhPxA+TKFxV";
    // `ssh-keygen -H` output for host `revoked.example.com` + KEY_A: the
    // real HashKnownHosts encoding, produced by OpenSSH itself.
    const HASHED_HOST: &str = "|1|01RQLI4rlZj1wcLWwYuQdiedcpQ=|OU6Ji5skpSnueFFtAwO5bfr7mPU=";

    fn key(base64: &str) -> PublicKey {
        keys::parse_public_key_base64(base64).expect("test key parses")
    }

    fn check(contents: &str, host: &str, port: u16, key_b64: &str) -> bool {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        key_is_revoked_in(f.path(), host, port, &key(key_b64)).unwrap()
    }

    #[test]
    fn exact_host_revocation_refuses_that_key_only() {
        let kh = format!("@revoked badhost.example ssh-ed25519 {KEY_A}\n");
        assert!(check(&kh, "badhost.example", 22, KEY_A));
        // A different key on the same host is not revoked.
        assert!(!check(&kh, "badhost.example", 22, KEY_B));
        // The same key on a different host is not revoked.
        assert!(!check(&kh, "otherhost.example", 22, KEY_A));
    }

    #[test]
    fn global_revocation_list_uses_star_pattern() {
        // The common CRL shape: revoke a compromised key everywhere.
        let kh = format!("@revoked * ssh-ed25519 {KEY_A}\n");
        assert!(check(&kh, "any.example", 22, KEY_A));
        assert!(check(&kh, "any.example", 2222, KEY_A));
        assert!(!check(&kh, "any.example", 22, KEY_B));
    }

    #[test]
    fn non_default_port_matches_bracket_form() {
        let kh = format!("@revoked [bad.example]:2203 ssh-ed25519 {KEY_A}\n");
        assert!(check(&kh, "bad.example", 2203, KEY_A));
        assert!(!check(&kh, "bad.example", 22, KEY_A));
    }

    #[test]
    fn negated_pattern_vetoes_the_list() {
        let kh = format!("@revoked *.example,!safe.example ssh-ed25519 {KEY_A}\n");
        assert!(check(&kh, "bad.example", 22, KEY_A));
        assert!(!check(&kh, "safe.example", 22, KEY_A));
    }

    #[test]
    fn hashed_entry_matches_via_hmac() {
        let kh = format!("@revoked {HASHED_HOST} ssh-ed25519 {KEY_A}\n");
        assert!(check(&kh, "revoked.example.com", 22, KEY_A));
        assert!(!check(&kh, "other.example.com", 22, KEY_A));
        assert!(!check(&kh, "revoked.example.com", 22, KEY_B));
    }

    #[test]
    fn ordinary_and_cert_authority_lines_are_ignored() {
        // Neither a plain entry nor a CA marker may register as revocation.
        let kh = format!(
            "host.example ssh-ed25519 {KEY_A}\n@cert-authority * ssh-ed25519 {KEY_A}\n"
        );
        assert!(!check(&kh, "host.example", 22, KEY_A));
    }

    #[test]
    fn missing_file_revokes_nothing() {
        let missing = Path::new("/nonexistent/known_hosts");
        assert!(!key_is_revoked_in(missing, "h.example", 22, &key(KEY_A)).unwrap());
    }

    #[test]
    fn hostname_match_is_case_insensitive() {
        let kh = format!("@revoked BadHost.Example ssh-ed25519 {KEY_A}\n");
        assert!(check(&kh, "badhost.example", 22, KEY_A));
    }

    fn ca_check(contents: &str, host: &str, port: u16) -> bool {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        cert_authority_matches_in(f.path(), host, port)
    }

    #[test]
    fn cert_authority_advisory_matches_by_pattern() {
        // The fleet-CA shape: one CA key trusted for a whole domain.
        let kh = format!("@cert-authority *.fleet.example ssh-ed25519 {KEY_A}\n");
        assert!(ca_check(&kh, "host1.fleet.example", 22));
        // As in OpenSSH, a bare pattern does not cover the bracketed
        // non-22 spelling; that needs its own [pattern]:port entry.
        assert!(!ca_check(&kh, "host1.fleet.example", 2222));
        assert!(ca_check(
            &format!("@cert-authority [*.fleet.example]:2222 ssh-ed25519 {KEY_A}\n"),
            "host1.fleet.example",
            2222
        ));
        assert!(!ca_check(&kh, "other.example", 22));
    }

    #[test]
    fn cert_authority_advisory_ignores_other_lines() {
        // Plain and @revoked lines must not trigger the CA advisory.
        let kh = format!(
            "host.example ssh-ed25519 {KEY_A}\n@revoked host.example ssh-ed25519 {KEY_A}\n"
        );
        assert!(!ca_check(&kh, "host.example", 22));
        // Missing file: quiet no.
        assert!(!cert_authority_matches_in(
            Path::new("/nonexistent/known_hosts"),
            "host.example",
            22
        ));
    }
}
