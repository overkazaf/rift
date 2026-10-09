//! Host-key verification against `~/.ssh/known_hosts`.
//!
//! Lookup (plain, `[host]:port` and hashed `|1|` entries) is delegated to
//! russh; the decision logic lives in [`classify`] so it can be tested
//! without a network.

use std::path::{Path, PathBuf};

use russh::keys::{ssh_key::PublicKey, HashAlg};

#[derive(Debug, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// The exact key is recorded for this host.
    Known,
    /// Nothing recorded for this algorithm. `other_keys` = the host has
    /// entries of a different key type.
    Unknown { other_keys: bool },
    /// A key of the same type is recorded and differs: possible MITM.
    /// (`line` comes from russh and undercounts comment lines: informational.)
    Mismatch { line: usize },
    /// The file could not be read or parsed; we cannot vouch for the key.
    Unreadable(String),
}

pub fn known_hosts_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".ssh").join("known_hosts")
}

/// Pure decision: compare `presented` with the recorded `(line, key)` pairs
/// that matched the host.
pub fn classify(recorded: &[(usize, PublicKey)], presented: &PublicKey) -> HostKeyStatus {
    let mut other = false;
    for (line, key) in recorded {
        if key.algorithm() == presented.algorithm() {
            if key == presented {
                return HostKeyStatus::Known;
            }
            return HostKeyStatus::Mismatch { line: *line };
        }
        other = true;
    }
    HostKeyStatus::Unknown { other_keys: other }
}

pub fn check(host: &str, port: u16, key: &PublicKey, path: &Path) -> HostKeyStatus {
    match russh::keys::known_hosts::known_host_keys_path(host, port, path) {
        Ok(recorded) => classify(&recorded, key),
        Err(e) => HostKeyStatus::Unreadable(e.to_string()),
    }
}

/// Append the key to known_hosts.
pub fn learn(host: &str, port: u16, key: &PublicKey, path: &Path) -> Result<(), String> {
    russh::keys::known_hosts::learn_known_hosts_path(host, port, key, path).map_err(|e| e.to_string())
}

/// `SHA256:...` fingerprint, as shown by `ssh-keygen -l`.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

pub fn key_type(key: &PublicKey) -> String {
    key.algorithm().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real keys from ssh-keygen: two ed25519 and one ecdsa (a different type).
    const ED_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIAosITxL2LkDCNXUgylBm4oj5v+6gHVNBvcE0qHeUmOn";
    const ED_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIG0IpVC8KNa8BxNIejO3q1jmZEGkjOn2/2zf3gxxYdN+";
    const ECDSA: &str = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBK7y9c+VhOawIeuolWTAf5mIzLnEHhPBUDcU3TtP734cU58Tlkc+etKRQvJHi+CkQGb+DmETdb5LyBC1tMBSuEg=";

    fn key(b64: &str) -> PublicKey {
        russh::keys::parse_public_key_base64(b64).expect("test key parses")
    }

    fn tmp(name: &str, content: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rift-kh-{}-{name}", std::process::id()));
        std::fs::write(&d, content).unwrap();
        d
    }

    #[test]
    fn classify_known_mismatch_unknown() {
        let (a, b, e) = (key(ED_A), key(ED_B), key(ECDSA));
        assert_eq!(classify(&[], &a), HostKeyStatus::Unknown { other_keys: false });
        assert_eq!(classify(&[(3, a.clone())], &a), HostKeyStatus::Known);
        assert_eq!(classify(&[(3, a.clone())], &b), HostKeyStatus::Mismatch { line: 3 });
        assert_eq!(classify(&[(1, e.clone())], &a), HostKeyStatus::Unknown { other_keys: true });
        // A matching key anywhere wins over unrelated entries.
        assert_eq!(classify(&[(1, e), (2, a.clone())], &a), HostKeyStatus::Known);
    }

    #[test]
    fn file_lookup_handles_ports_comments_and_missing_file() {
        let a = key(ED_A);
        let b = key(ED_B);
        let p = tmp("lookup", &format!(
            "# comment\nexample.com ssh-ed25519 {ED_A}\n[ex.org]:2222 ssh-ed25519 {ED_B}\n"
        ));
        assert_eq!(check("example.com", 22, &a, &p), HostKeyStatus::Known);
        // (russh's line numbers skip comment lines, so only the variant is asserted.)
        assert!(matches!(check("example.com", 22, &b, &p), HostKeyStatus::Mismatch { .. }));
        assert_eq!(check("ex.org", 2222, &b, &p), HostKeyStatus::Known);
        // Port is part of the identity: [ex.org]:22 is a different host.
        assert_eq!(check("ex.org", 22, &b, &p), HostKeyStatus::Unknown { other_keys: false });
        assert_eq!(check("other.net", 22, &a, &p), HostKeyStatus::Unknown { other_keys: false });
        let _ = std::fs::remove_file(&p);
        assert_eq!(check("example.com", 22, &a, Path::new("/nonexistent/known_hosts")), HostKeyStatus::Unknown { other_keys: false });
    }

    #[test]
    fn learn_then_check_round_trips() {
        let a = key(ED_A);
        let p = std::env::temp_dir().join(format!("rift-kh-{}-learn", std::process::id()));
        let _ = std::fs::remove_file(&p);
        learn("new.host", 2200, &a, &p).unwrap();
        assert_eq!(check("new.host", 2200, &a, &p), HostKeyStatus::Known);
        assert_eq!(check("new.host", 22, &a, &p), HostKeyStatus::Unknown { other_keys: false });
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn fingerprint_looks_like_openssh() {
        let f = fingerprint(&key(ED_A));
        assert!(f.starts_with("SHA256:") && f.len() == 7 + 43, "{f}");
        assert_eq!(key_type(&key(ED_A)), "ssh-ed25519");
    }
}
