//! `ekapkgs lease connect`: build on a machine that granted a lease.
//!
//! Redeems the ticket, then listens on a local Unix socket. Nix uses the
//! socket as a remote builder (`unix://…` in `builders`), and each
//! connection to it becomes one stream to the lender's daemon.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use color_eyre::eyre::{WrapErr, bail, eyre};
use ekapkgs_nix::NixCommand;
use iroh::endpoint::Connection;
use serde::Deserialize;
use tokio::net::UnixListener;
use tokio::task::JoinSet;

use super::ticket::Ticket;
use super::wire;

/// What `ekapkgs lease connect` was asked for.
pub struct ConnectOptions {
    /// The ticket, or `None`/`-` to read it from stdin.
    pub ticket: Option<String>,
    pub socket: Option<PathBuf>,
    pub relay: bool,
    /// Command to run with the lease as its Nix builder.
    pub command: Vec<String>,
}

pub fn execute(opts: ConnectOptions) -> color_eyre::Result<()> {
    let ticket = Ticket::decode(&read_ticket(opts.ticket.as_deref())?)?;
    if ticket.expires_at <= super::unix_now() {
        bail!(
            "this lease expired at {}",
            super::format_time(ticket.expires_at)
        );
    }
    warn_if_untrusted();

    let socket = match opts.socket {
        Some(path) => path,
        None => default_socket_path(&ticket)?,
    };

    let rt = tokio::runtime::Runtime::new()?;
    let status = rt.block_on(async {
        let endpoint = super::bind_endpoint(opts.relay, Vec::new()).await?;
        tracing::info!("Connecting to lender {}...", ticket.addr.id.fmt_short());
        let conn = endpoint
            .connect(ticket.addr.clone(), wire::ALPN)
            .await
            .map_err(|e| eyre!("could not reach the lender: {e}"))?;
        let expires_at = wire::redeem(&conn, &ticket.secret_bytes()?).await?;

        let listener = bind_socket(&socket)?;
        let builders = ticket.builders_line(&socket);
        tracing::info!(
            "Lease active until {} ({} jobs)",
            super::format_time(expires_at),
            ticket.jobs
        );

        let lease_end = tokio::time::sleep(super::until(expires_at));
        let result = if opts.command.is_empty() {
            tracing::info!("Use the lease as a builder with:");
            println!("--builders '{builders}'");
            tracing::info!("Press Ctrl-C to end the lease.");
            let stop = async {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    () = lease_end => tracing::info!("Lease expired"),
                }
            };
            proxy(&listener, &conn, stop).await.map(|()| None)
        } else {
            run_with_lease(&opts.command, &builders, &listener, &conn, lease_end)
                .await
                .map(Some)
        };

        let _ = std::fs::remove_file(&socket);
        conn.close(wire::CLOSE_ENDED.into(), b"lease done");
        endpoint.close().await;
        result
    })?;

    match status {
        Some(status) if !status.success() => {
            // Pass the command's failure on, like `nix shell --command`.
            std::process::exit(status.code().unwrap_or(1));
        },
        _ => Ok(()),
    }
}

/// Run `command` with the lease as Nix's only builder, proxying builds until
/// it exits. Fails if the lease ends first.
async fn run_with_lease(
    command: &[String],
    builders: &str,
    listener: &UnixListener,
    conn: &Connection,
    lease_end: tokio::time::Sleep,
) -> color_eyre::Result<ExitStatus> {
    let mut nix_config = std::env::var("NIX_CONFIG").unwrap_or_default();
    nix_config.push_str(&format!(
        "\nbuilders = {builders}\nbuilders-use-substitutes = true\n"
    ));

    let mut child = tokio::process::Command::new(&command[0])
        .args(&command[1..])
        .env("NIX_CONFIG", nix_config)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| eyre!("failed to run {}: {e}", command[0]))?;

    let mut status = None;
    let stop = async {
        tokio::select! {
            result = child.wait() => status = Some(result),
            () = lease_end => {},
        }
    };
    proxy(listener, conn, stop).await?;
    match status {
        Some(result) => Ok(result?),
        None => bail!("the lease expired before `{}` finished", command.join(" ")),
    }
}

/// Forward every connection on `listener` to the lender until `stop`
/// completes or the lender goes away.
async fn proxy(
    listener: &UnixListener,
    conn: &Connection,
    stop: impl std::future::Future<Output = ()>,
) -> color_eyre::Result<()> {
    let mut streams = JoinSet::new();
    tokio::pin!(stop);
    let result = loop {
        tokio::select! {
            () = &mut stop => break Ok(()),
            reason = conn.closed() => break Err(eyre!("the lender ended the lease: {reason}")),
            accepted = listener.accept() => {
                let (local, _) = accepted?;
                let conn = conn.clone();
                streams.spawn(async move {
                    match conn.open_bi().await {
                        Ok((send, recv)) => {
                            let (read, write) = local.into_split();
                            wire::splice(send, recv, read, write).await;
                        },
                        Err(e) => tracing::warn!("Could not open a stream to the lender: {e}"),
                    }
                });
            },
            Some(_) = streams.join_next(), if !streams.is_empty() => {},
        }
    };
    streams.shutdown().await;
    result
}

fn read_ticket(arg: Option<&str>) -> color_eyre::Result<String> {
    match arg {
        Some(ticket) if ticket != "-" => Ok(ticket.to_owned()),
        _ => {
            if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                eprintln!("Paste the lease ticket and press Enter:");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                Ok(line)
            } else {
                let mut ticket = String::new();
                std::io::stdin().read_to_string(&mut ticket)?;
                Ok(ticket)
            }
        },
    }
}

/// `$XDG_RUNTIME_DIR/ekapkgs/lease-<lender>.sock`, in a directory only this
/// user can enter.
fn default_socket_path(ticket: &Ticket) -> color_eyre::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("ekapkgs");
    std::fs::create_dir_all(&dir)
        .wrap_err_with(|| format!("failed to create {}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir.join(format!("lease-{}.sock", ticket.addr.id.fmt_short())))
}

/// Listen on `path`, replacing a stale socket left by an earlier run.
fn bind_socket(path: &Path) -> color_eyre::Result<UnixListener> {
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            bail!(
                "{} is in use; is another `ekapkgs lease connect` running for this lender?",
                path.display()
            );
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)
        .wrap_err_with(|| format!("failed to listen on {}", path.display()))?;
    // Only this user (and root, which runs the Nix daemon) may connect.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// The Nix daemon ignores `builders` from users it doesn't trust.
fn warn_if_untrusted() {
    #[derive(Deserialize)]
    struct StoreInfo {
        trusted: Option<bool>,
    }
    let info: Result<StoreInfo, _> = NixCommand::new(&["store", "info"]).arg("--json").json();
    if let Ok(StoreInfo {
        trusted: Some(false),
    }) = info
    {
        tracing::warn!(
            "You are not a trusted Nix user here, so the daemon will ignore the lease's \
             `builders` setting. Add yourself to `trusted-users` to build on the lease."
        );
    }
}
