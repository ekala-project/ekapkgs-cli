//! The lease ticket: everything a borrower needs to redeem a lease.
//!
//! A ticket is `ekalease1` followed by base64url (no padding) of a JSON
//! document. It holds the redemption secret, so whoever has the ticket can
//! redeem the lease. Share it privately.

use std::path::Path;

use color_eyre::eyre::{WrapErr, bail, eyre};
use data_encoding::BASE64URL_NOPAD;
use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};

/// Prefix of every encoded ticket. The digit is the ticket format version.
pub const PREFIX: &str = "ekalease1";

/// Length of the redemption secret in bytes.
pub const SECRET_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    /// The lender's iroh endpoint: its public key, relay and direct addresses.
    pub addr: EndpointAddr,
    /// One-time redemption secret, base64url without padding.
    pub secret: String,
    /// Unix time (seconds) after which the lender stops serving the lease.
    pub expires_at: u64,
    /// Nix system types the lender builds, e.g. `x86_64-linux`.
    pub systems: Vec<String>,
    /// Nix system features the lender supports, e.g. `big-parallel`.
    pub features: Vec<String>,
    /// Builds the borrower should run on the lender at once.
    pub jobs: u32,
}

impl Ticket {
    pub fn encode(&self) -> color_eyre::Result<String> {
        let json = serde_json::to_vec(self)?;
        Ok(format!("{PREFIX}{}", BASE64URL_NOPAD.encode(&json)))
    }

    pub fn decode(s: &str) -> color_eyre::Result<Self> {
        let s = s.trim();
        let Some(payload) = s.strip_prefix(PREFIX) else {
            bail!("not an ekapkgs lease ticket (expected it to start with `{PREFIX}`)");
        };
        let json = BASE64URL_NOPAD
            .decode(payload.as_bytes())
            .map_err(|e| eyre!("lease ticket is corrupt: {e}"))?;
        let ticket: Self = serde_json::from_slice(&json).wrap_err("lease ticket is corrupt")?;
        ticket.secret_bytes()?;
        Ok(ticket)
    }

    pub fn secret_bytes(&self) -> color_eyre::Result<[u8; SECRET_LEN]> {
        let bytes = BASE64URL_NOPAD
            .decode(self.secret.as_bytes())
            .map_err(|e| eyre!("lease ticket secret is corrupt: {e}"))?;
        bytes
            .try_into()
            .map_err(|_| eyre!("lease ticket secret must be {SECRET_LEN} bytes"))
    }

    /// A line in Nix's machines format that builds on the lease through the
    /// local socket at `socket`.
    ///
    /// Fields: store URI, systems, SSH key, max jobs, speed factor,
    /// supported features, mandatory features.
    pub fn builders_line(&self, socket: &Path) -> String {
        let list = |items: &[String]| {
            if items.is_empty() {
                "-".to_owned()
            } else {
                items.join(",")
            }
        };
        format!(
            "unix://{} {} - {} 1 {} -",
            socket.display(),
            list(&self.systems),
            self.jobs.max(1),
            list(&self.features),
        )
    }
}

/// Generate a random redemption secret.
pub fn generate_secret() -> color_eyre::Result<[u8; SECRET_LEN]> {
    let mut secret = [0u8; SECRET_LEN];
    getrandom::fill(&mut secret).map_err(|e| eyre!("failed to generate lease secret: {e}"))?;
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Ticket {
        let key = iroh::SecretKey::from_bytes(&[7u8; 32]);
        Ticket {
            addr: EndpointAddr::new(key.public()),
            secret: BASE64URL_NOPAD.encode(&[1u8; SECRET_LEN]),
            expires_at: 1_800_000_000,
            systems: vec!["x86_64-linux".to_owned(), "i686-linux".to_owned()],
            features: vec!["big-parallel".to_owned()],
            jobs: 4,
        }
    }

    #[test]
    fn round_trips() {
        let ticket = sample();
        let encoded = ticket.encode().unwrap();
        assert!(encoded.starts_with(PREFIX));
        assert_eq!(Ticket::decode(&encoded).unwrap(), ticket);
        // Surrounding whitespace from copy-paste is ignored.
        assert_eq!(Ticket::decode(&format!("  {encoded}\n")).unwrap(), ticket);
    }

    #[test]
    fn rejects_foreign_and_corrupt_tickets() {
        assert!(Ticket::decode("endpointabc").is_err());
        assert!(Ticket::decode("ekalease1!!!").is_err());

        let mut short = sample();
        short.secret = BASE64URL_NOPAD.encode(&[1u8; 8]);
        assert!(Ticket::decode(&short.encode().unwrap()).is_err());
    }

    #[test]
    fn builders_line_has_all_machine_fields() {
        let mut ticket = sample();
        let line = ticket.builders_line(Path::new("/run/user/1000/ekapkgs/lease-ab.sock"));
        assert_eq!(
            line,
            "unix:///run/user/1000/ekapkgs/lease-ab.sock x86_64-linux,i686-linux - 4 1 \
             big-parallel -"
        );

        ticket.features.clear();
        ticket.jobs = 0;
        let line = ticket.builders_line(Path::new("/s.sock"));
        assert_eq!(line, "unix:///s.sock x86_64-linux,i686-linux - 1 1 - -");
    }
}
