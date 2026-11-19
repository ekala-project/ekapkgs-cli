//! `ekapkgs lease grant`: lend this machine's Nix builder for a while.
//!
//! Every session the borrower opens runs `nix daemon --stdio
//! --force-untrusted --process-ops`, which applies the daemon's untrusted-client rules
//! (no unsigned input-addressed paths, no restricted settings) before
//! handing the work to the local store. Builds run in the Nix sandbox as
//! they would for any unprivileged local user.

use std::ffi::OsString;
use std::future::Future;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use color_eyre::eyre::{bail, eyre};
use data_encoding::BASE64URL_NOPAD;
use ekapkgs_nix::NixCommand;
use iroh::endpoint::Incoming;
use iroh::{Endpoint, EndpointId};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::ticket::{SECRET_LEN, Ticket, generate_secret};
use super::wire::{self, Status};

/// What `ekapkgs lease grant` was asked for.
pub struct GrantOptions {
    pub ttl: Duration,
    pub jobs: Option<u32>,
    pub systems: Vec<String>,
    pub features: Vec<String>,
    pub relay: bool,
}

/// The program each borrower session runs, fed the session's bytes on
/// stdin and stdout.
#[derive(Debug, Clone)]
pub struct SessionCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
}

impl SessionCommand {
    /// `nix daemon --stdio --force-untrusted --process-ops`.
    /// `--force-untrusted` sits behind the `daemon-trust-override`
    /// experimental feature, and some Nix versions put `--process-ops`
    /// behind `mounted-ssh-store`.
    ///
    /// `--process-ops` is what makes `--force-untrusted` stick: without it,
    /// some Nix versions (e.g. Determinate Nix 3.21) forward a stdio
    /// session to the system daemon unchanged, which then trusts the
    /// borrower as much as the lending user.
    pub fn nix_daemon() -> Self {
        Self {
            program: "nix".into(),
            args: [
                "--extra-experimental-features",
                "nix-command daemon-trust-override mounted-ssh-store",
                "daemon",
                "--stdio",
                "--force-untrusted",
                "--process-ops",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        }
    }

    fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }
}

/// The state of one lease on the lender.
pub struct Lease {
    secret: [u8; SECRET_LEN],
    expires_at: u64,
    deadline: Instant,
    session: SessionCommand,
    /// Caps concurrent daemon sessions.
    sessions: Arc<Semaphore>,
    /// The borrower that redeemed the lease. Only it may connect afterwards.
    redeemed_by: Mutex<Option<EndpointId>>,
}

impl Lease {
    pub fn new(
        secret: [u8; SECRET_LEN],
        ttl: Duration,
        session: SessionCommand,
        max_sessions: usize,
    ) -> Self {
        Self {
            secret,
            expires_at: super::unix_now() + ttl.as_secs(),
            deadline: Instant::now() + ttl,
            session,
            sessions: Arc::new(Semaphore::new(max_sessions.max(1))),
            redeemed_by: Mutex::new(None),
        }
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Decide whether `remote`, presenting `secret`, may use the lease.
    fn admit(&self, remote: EndpointId, secret: &[u8; SECRET_LEN]) -> Status {
        if Instant::now() >= self.deadline {
            return Status::Expired;
        }
        if !bool::from(self.secret.ct_eq(secret)) {
            return Status::BadSecret;
        }
        let mut redeemed_by = self.redeemed_by.lock().expect("lease lock poisoned");
        match *redeemed_by {
            None => {
                *redeemed_by = Some(remote);
                tracing::info!("Lease redeemed by {}", remote.fmt_short());
                Status::Ok
            },
            // The borrower reconnecting, e.g. after a network change.
            Some(id) if id == remote => Status::Ok,
            Some(_) => Status::AlreadyRedeemed,
        }
    }
}

pub fn execute(opts: GrantOptions) -> color_eyre::Result<()> {
    if opts.ttl.is_zero() {
        bail!("--ttl must be longer than zero");
    }
    let session = SessionCommand::nix_daemon();
    preflight(&session)?;

    let systems = if opts.systems.is_empty() {
        local_systems()?
    } else {
        opts.systems
    };
    let features = if opts.features.is_empty() {
        local_features()?
    } else {
        opts.features
    };
    let jobs = match opts.jobs {
        Some(jobs) => jobs.max(1),
        None => local_max_jobs()?,
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let endpoint = super::bind_endpoint(opts.relay, vec![wire::ALPN.to_vec()]).await?;
        super::wait_for_address(&endpoint, opts.relay).await;

        let secret = generate_secret()?;
        // A few spare sessions for the queries Nix makes besides builds.
        let max_sessions = jobs as usize * 2 + 2;
        let lease = Arc::new(Lease::new(secret, opts.ttl, session, max_sessions));

        let ticket = Ticket {
            addr: endpoint.addr(),
            secret: BASE64URL_NOPAD.encode(&secret),
            expires_at: lease.expires_at(),
            systems,
            features,
            jobs,
        };

        tracing::info!(
            "Lending this machine's Nix builder until {} ({} jobs, {})",
            super::format_time(ticket.expires_at),
            ticket.jobs,
            ticket.systems.join(", "),
        );
        tracing::warn!(
            "Until the lease ends, the borrower can run sandboxed builds here as an untrusted Nix \
             user and read any path in this machine's Nix store."
        );
        tracing::info!("Share this ticket privately. It can be redeemed once:");
        println!("{}", ticket.encode()?);
        tracing::info!(
            "On the borrowing machine: ekapkgs lease connect -- nix build <installable>"
        );
        tracing::info!("Press Ctrl-C to end the lease early.");

        let stop = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        let reason = serve(endpoint, lease, stop).await;
        tracing::info!("Lease ended ({reason})");
        Ok(())
    })
}

/// Serve `lease` on `endpoint` until it expires or `stop` completes.
/// Returns why it ended. Every connection and session is torn down before
/// returning.
pub async fn serve(
    endpoint: Endpoint,
    lease: Arc<Lease>,
    stop: impl Future<Output = ()>,
) -> &'static str {
    let mut connections = JoinSet::new();
    tokio::pin!(stop);

    let reason = loop {
        tokio::select! {
            () = tokio::time::sleep_until(lease.deadline) => break "expired",
            () = &mut stop => break "stopped",
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break "endpoint closed" };
                let lease = lease.clone();
                connections.spawn(async move {
                    if let Err(e) = handle_connection(incoming, lease).await {
                        tracing::debug!("lease connection ended: {e:#}");
                    }
                });
            },
            // Reap finished connection tasks so the set doesn't grow.
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
        }
    };

    // Dropping the tasks drops their sessions, which kills the daemons.
    connections.shutdown().await;
    endpoint.close().await;
    reason
}

async fn handle_connection(incoming: Incoming, lease: Arc<Lease>) -> color_eyre::Result<()> {
    let conn = incoming.await?;
    let remote = conn.remote_id();

    let (mut send, mut recv) = tokio::time::timeout(wire::HANDSHAKE_TIMEOUT, conn.accept_bi())
        .await
        .map_err(|_| eyre!("{} opened no control stream", remote.fmt_short()))??;
    let secret = wire::read_redemption(&mut recv).await?;
    let status = lease.admit(remote, &secret);
    wire::send_status(&mut send, status, lease.expires_at).await?;
    if status != Status::Ok {
        tracing::warn!(
            "Refused lease connection from {}: {status:?}",
            remote.fmt_short()
        );
        // Give the reply a moment to reach the peer before closing.
        let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
        conn.close(wire::CLOSE_DENIED.into(), b"lease denied");
        return Ok(());
    }

    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            stream = conn.accept_bi() => {
                let (send, recv) = match stream {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::debug!("borrower connection closed: {e}");
                        break;
                    },
                };
                let permit = lease.sessions.clone().acquire_owned().await?;
                let session = lease.session.clone();
                sessions.spawn(async move {
                    let _permit = permit;
                    if let Err(e) = run_session(&session, send, recv).await {
                        tracing::warn!("Lease session failed: {e:#}");
                    }
                });
            },
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {},
        }
    }
    sessions.shutdown().await;
    Ok(())
}

/// Run one daemon session, wiring its stdin and stdout to the stream pair.
async fn run_session(
    session: &SessionCommand,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
) -> color_eyre::Result<()> {
    let mut child = session
        .command()
        .spawn()
        .map_err(|e| eyre!("failed to start {:?}: {e}", session.program))?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let log = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!("lease session: {line}");
        }
    });

    tracing::debug!("lease session started");
    wire::splice(send, recv, stdout, stdin).await;
    let status = child.wait().await?;
    let _ = log.await;
    tracing::debug!("lease session finished ({status})");
    Ok(())
}

/// Check that the local Nix can run untrusted daemon sessions.
fn preflight(session: &SessionCommand) -> color_eyre::Result<()> {
    // With stdin at end of file the daemon exits during the handshake. It
    // only mentions the flag if it doesn't understand it.
    let output = std::process::Command::new(&session.program)
        .args(&session.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| eyre!("failed to run {:?}: {e}", session.program))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if [
        "force-untrusted",
        "process-ops",
        "daemon-trust-override",
        "mounted-ssh-store",
    ]
    .iter()
    .any(|flag| stderr.contains(flag))
    {
        bail!(
            "this Nix can't run untrusted daemon sessions (`nix daemon --stdio --force-untrusted \
             --process-ops`):\n{}",
            stderr.trim()
        );
    }
    Ok(())
}

fn nix_setting(name: &str) -> color_eyre::Result<String> {
    let output = NixCommand::new(&["config", "show"]).arg(name).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// This machine's system and its extra platforms.
fn local_systems() -> color_eyre::Result<Vec<String>> {
    let mut systems = vec![nix_setting("system")?];
    for platform in nix_setting("extra-platforms")?.split_whitespace() {
        if !systems.iter().any(|s| s == platform) {
            systems.push(platform.to_owned());
        }
    }
    Ok(systems)
}

fn local_features() -> color_eyre::Result<Vec<String>> {
    Ok(nix_setting("system-features")?
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

fn local_max_jobs() -> color_eyre::Result<u32> {
    let value = nix_setting("max-jobs")?;
    let jobs = match value.parse::<u32>() {
        Ok(jobs) => jobs,
        // `auto` means one per CPU.
        Err(_) => std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
    };
    Ok(jobs.max(1))
}
