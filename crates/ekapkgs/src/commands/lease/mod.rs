//! One-time build leases: lend a machine's Nix builder to another machine
//! for a fixed time.
//!
//! The lender runs `ekapkgs lease grant` and shares the ticket it prints.
//! The borrower runs `ekapkgs lease connect`, which redeems the ticket over
//! an iroh connection and exposes the lender's daemon to Nix as a `unix://`
//! builder. A ticket can be redeemed once. After that, only the endpoint
//! that redeemed it may connect, until the lease expires.

mod connect;
mod grant;
mod ticket;
mod wire;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{bail, eyre};
use iroh::endpoint::{RelayMode, presets};
use iroh::{Endpoint, RelayUrl};

use crate::cli::LeaseCommand;

pub fn execute(command: LeaseCommand) -> color_eyre::Result<()> {
    match command {
        LeaseCommand::Grant {
            ttl,
            jobs,
            systems,
            features,
            relays,
            no_relay,
        } => grant::execute(grant::GrantOptions {
            ttl: parse_duration(&ttl)?,
            jobs,
            systems,
            features,
            relays: Relays::from_args(&relays, no_relay)?.unwrap_or(Relays::Default),
        }),
        LeaseCommand::Connect {
            ticket,
            socket,
            relays,
            no_relay,
            command,
        } => connect::execute(connect::ConnectOptions {
            ticket,
            socket,
            relays: Relays::from_args(&relays, no_relay)?,
            command,
        }),
    }
}

/// Which relay servers an endpoint uses to get through NAT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relays {
    /// The public n0 relays.
    Default,
    /// No relays: direct addresses only.
    Disabled,
    /// These relays, e.g. a self-hosted `iroh-relay`.
    Custom(Vec<RelayUrl>),
}

impl Relays {
    /// The relays chosen on the command line, or `None` if neither
    /// `--relay` nor `--no-relay` was given.
    fn from_args(urls: &[String], no_relay: bool) -> color_eyre::Result<Option<Self>> {
        if no_relay {
            return Ok(Some(Self::Disabled));
        }
        if urls.is_empty() {
            return Ok(None);
        }
        let urls = urls
            .iter()
            .map(|url| {
                url.parse::<RelayUrl>()
                    .map_err(|e| eyre!("invalid relay URL `{url}`: {e}"))
            })
            .collect::<color_eyre::Result<_>>()?;
        Ok(Some(Self::Custom(urls)))
    }

    fn enabled(&self) -> bool {
        *self != Self::Disabled
    }

    fn mode(&self) -> RelayMode {
        match self {
            Self::Default => RelayMode::Default,
            Self::Disabled => RelayMode::Disabled,
            Self::Custom(urls) => RelayMode::custom(urls.iter().cloned()),
        }
    }
}

/// Bind an iroh endpoint. It never publishes its address to a discovery
/// service: the lender's address travels in the ticket.
async fn bind_endpoint(relays: &Relays, alpns: Vec<Vec<u8>>) -> color_eyre::Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::Minimal).relay_mode(relays.mode());
    if !alpns.is_empty() {
        builder = builder.alpns(alpns);
    }
    builder
        .bind()
        .await
        .map_err(|e| eyre!("failed to start the iroh endpoint: {e}"))
}

/// Wait until the endpoint knows enough addresses to be worth putting in a
/// ticket: a relay when relays are on, otherwise a direct address.
async fn wait_for_address(endpoint: &Endpoint, relays: &Relays) {
    const TIMEOUT: Duration = Duration::from_secs(20);
    if relays.enabled() {
        if tokio::time::timeout(TIMEOUT, endpoint.online())
            .await
            .is_err()
        {
            tracing::warn!("No relay reachable; the ticket only has direct addresses");
        }
        return;
    }
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    while endpoint.addr().addrs.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if endpoint.addr().addrs.is_empty() {
        tracing::warn!("This machine has no usable addresses; the borrower won't reach it");
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Time left until the Unix time `at`.
fn until(at: u64) -> Duration {
    Duration::from_secs(at.saturating_sub(unix_now()))
}

/// Local wall-clock time of the Unix time `at`.
fn format_time(at: u64) -> String {
    let Ok(ts) = jiff::Timestamp::from_second(at as i64) else {
        return at.to_string();
    };
    ts.to_zoned(jiff::tz::TimeZone::system())
        .strftime("%Y-%m-%d %H:%M:%S %Z")
        .to_string()
}

/// Parse a duration like `90s`, `30m`, `2h` or `1d`.
fn parse_duration(s: &str) -> color_eyre::Result<Duration> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (digits, unit) = s.split_at(split);
    let Ok(n) = digits.parse::<u64>() else {
        bail!("invalid duration `{s}` (expected e.g. 30m, 2h, 1d)");
    };
    let secs = match unit {
        "s" => 1,
        "" | "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => bail!("invalid duration unit in `{s}` (use s, m, h or d)"),
    };
    Ok(Duration::from_secs(n.saturating_mul(secs)))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use data_encoding::BASE64URL_NOPAD;

    use super::grant::{Lease, SessionCommand, serve};
    use super::ticket::Ticket;
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(2700));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("2w").is_err());
        assert!(parse_duration("-1h").is_err());
    }

    /// A lender whose sessions run `cat`, so each stream echoes.
    async fn echo_lender(secret: [u8; 32], ttl: Duration) -> (Endpoint, Arc<Lease>) {
        let endpoint = bind_endpoint(&Relays::Disabled, vec![wire::ALPN.to_vec()])
            .await
            .unwrap();
        wait_for_address(&endpoint, &Relays::Disabled).await;
        let session = SessionCommand {
            program: "cat".into(),
            args: Vec::new(),
        };
        let lease = Arc::new(Lease::new(secret, ttl, session, 4));
        (endpoint, lease)
    }

    async fn echo(conn: &iroh::endpoint::Connection, msg: &[u8]) -> Vec<u8> {
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(msg).await.unwrap();
        send.finish().unwrap();
        recv.read_to_end(1 << 20).await.unwrap()
    }

    #[tokio::test]
    async fn lease_redeems_once_and_carries_sessions() {
        let secret = [9u8; 32];
        let (lender, lease) = echo_lender(secret, Duration::from_secs(60)).await;
        let addr = lender.addr();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(lender, lease, async {
            let _ = stop_rx.await;
        }));

        // A wrong secret is refused and doesn't use up the lease.
        let intruder = bind_endpoint(&Relays::Disabled, Vec::new()).await.unwrap();
        let conn = intruder.connect(addr.clone(), wire::ALPN).await.unwrap();
        let err = wire::redeem(&conn, &[0u8; 32]).await.unwrap_err();
        assert!(err.to_string().contains("rejected"), "{err}");

        // The borrower redeems it and runs two sessions at once.
        let borrower = bind_endpoint(&Relays::Disabled, Vec::new()).await.unwrap();
        let conn = borrower.connect(addr.clone(), wire::ALPN).await.unwrap();
        wire::redeem(&conn, &secret).await.unwrap();
        let (a, b) = tokio::join!(echo(&conn, b"first session"), echo(&conn, b"second"));
        assert_eq!(a, b"first session");
        assert_eq!(b, b"second");

        // The same borrower may reconnect.
        let again = borrower.connect(addr.clone(), wire::ALPN).await.unwrap();
        wire::redeem(&again, &secret).await.unwrap();
        assert_eq!(echo(&again, b"reconnected").await, b"reconnected");

        // Another machine holding the same ticket may not.
        let other = bind_endpoint(&Relays::Disabled, Vec::new()).await.unwrap();
        let conn = other.connect(addr, wire::ALPN).await.unwrap();
        let err = wire::redeem(&conn, &secret).await.unwrap_err();
        assert!(err.to_string().contains("already redeemed"), "{err}");

        stop_tx.send(()).unwrap();
        assert_eq!(server.await.unwrap(), "stopped");
    }

    #[tokio::test]
    async fn lease_ends_at_its_deadline() {
        let secret = [3u8; 32];
        let (lender, lease) = echo_lender(secret, Duration::from_secs(2)).await;
        let addr = lender.addr();
        let server = tokio::spawn(serve(lender, lease, std::future::pending()));

        let borrower = bind_endpoint(&Relays::Disabled, Vec::new()).await.unwrap();
        let conn = borrower.connect(addr, wire::ALPN).await.unwrap();
        wire::redeem(&conn, &secret).await.unwrap();

        // A session left open past the deadline is cut off.
        let (_send, mut recv) = conn.open_bi().await.unwrap();
        let reason = tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("lease outlived its deadline")
            .unwrap();
        assert_eq!(reason, "expired");
        let rest = recv.read_to_end(1 << 20).await;
        assert!(rest.is_err() || rest.unwrap().is_empty());
        tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .expect("connection still open after the lease ended");
    }

    #[test]
    fn relay_args() {
        assert_eq!(Relays::from_args(&[], false).unwrap(), None);
        assert_eq!(
            Relays::from_args(&[], true).unwrap(),
            Some(Relays::Disabled)
        );
        let url = "https://relay.example.com".to_owned();
        assert_eq!(
            Relays::from_args(std::slice::from_ref(&url), false).unwrap(),
            Some(Relays::Custom(vec![url.parse().unwrap()]))
        );
        assert!(Relays::from_args(&["not a url".to_owned()], false).is_err());
    }

    #[test]
    fn ticket_secret_matches_lease_secret_encoding() {
        let secret = [5u8; 32];
        let key = iroh::SecretKey::from_bytes(&[1u8; 32]);
        let ticket = Ticket {
            addr: iroh::EndpointAddr::new(key.public()),
            secret: BASE64URL_NOPAD.encode(&secret),
            expires_at: 0,
            systems: Vec::new(),
            features: Vec::new(),
            jobs: 1,
        };
        assert_eq!(ticket.secret_bytes().unwrap(), secret);
    }
}
