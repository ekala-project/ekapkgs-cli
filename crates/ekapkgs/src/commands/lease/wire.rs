//! The lease protocol on top of an iroh connection.
//!
//! The borrower opens the connection with ALPN [`ALPN`]. Its first
//! bidirectional stream is the control stream: it sends [`MAGIC`] and the
//! ticket's secret, and the lender answers with a status byte and the lease
//! expiry. Every bidirectional stream after that carries one Nix daemon
//! session (the worker protocol, byte for byte).

use std::time::Duration;

use color_eyre::eyre::{bail, eyre};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use super::ticket::SECRET_LEN;

pub const ALPN: &[u8] = b"ekapkgs/lease/1";

/// First bytes of the control stream.
pub const MAGIC: &[u8; 8] = b"EKLEASE1";

/// How long either side waits for the control exchange.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// QUIC close code for a refused redemption.
pub const CLOSE_DENIED: u32 = 403;
/// QUIC close code sent when the lease ends.
pub const CLOSE_ENDED: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Ok = 0,
    BadSecret = 1,
    AlreadyRedeemed = 2,
    Expired = 3,
}

impl Status {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Ok),
            1 => Some(Self::BadSecret),
            2 => Some(Self::AlreadyRedeemed),
            3 => Some(Self::Expired),
            _ => None,
        }
    }
}

/// Borrower side: redeem the lease on `conn`. Returns the lease expiry
/// (Unix seconds) reported by the lender.
pub async fn redeem(conn: &Connection, secret: &[u8; SECRET_LEN]) -> color_eyre::Result<u64> {
    let exchange = async {
        let (mut send, mut recv) = conn.open_bi().await?;
        let mut hello = Vec::with_capacity(MAGIC.len() + SECRET_LEN);
        hello.extend_from_slice(MAGIC);
        hello.extend_from_slice(secret);
        send.write_all(&hello).await?;
        send.finish()?;

        let mut reply = [0u8; 9];
        recv.read_exact(&mut reply).await?;
        Ok::<_, color_eyre::Report>(reply)
    };
    let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, exchange)
        .await
        .map_err(|_| eyre!("lender did not answer the lease redemption in time"))??;

    let expires_at = u64::from_le_bytes(reply[1..].try_into().expect("8 bytes"));
    match Status::from_byte(reply[0]) {
        Some(Status::Ok) => Ok(expires_at),
        Some(Status::BadSecret) => bail!("the lender rejected this ticket's secret"),
        Some(Status::AlreadyRedeemed) => {
            bail!("this lease ticket was already redeemed by another machine")
        },
        Some(Status::Expired) => bail!("this lease has expired"),
        None => bail!("lender sent an unknown lease status {}", reply[0]),
    }
}

/// Lender side: read the borrower's redemption from the control stream.
/// Returns the secret it presented.
pub async fn read_redemption(recv: &mut RecvStream) -> color_eyre::Result<[u8; SECRET_LEN]> {
    let mut hello = [0u8; MAGIC.len() + SECRET_LEN];
    tokio::time::timeout(HANDSHAKE_TIMEOUT, recv.read_exact(&mut hello))
        .await
        .map_err(|_| eyre!("borrower did not send a redemption in time"))??;
    if &hello[..MAGIC.len()] != MAGIC {
        bail!("borrower spoke an unknown lease protocol");
    }
    Ok(hello[MAGIC.len()..].try_into().expect("secret length"))
}

/// Lender side: answer a redemption.
pub async fn send_status(
    send: &mut SendStream,
    status: Status,
    expires_at: u64,
) -> color_eyre::Result<()> {
    let mut reply = [0u8; 9];
    reply[0] = status as u8;
    reply[1..].copy_from_slice(&expires_at.to_le_bytes());
    send.write_all(&reply).await?;
    send.finish()?;
    Ok(())
}

/// Copy bytes both ways between a QUIC stream pair and a local
/// reader/writer pair until both directions finish.
///
/// When the local side reaches end of file the QUIC send stream is
/// finished, and when the peer finishes its stream the local writer is
/// shut down, so each side sees a clean end of file.
pub async fn splice<R, W>(
    mut send: SendStream,
    mut recv: RecvStream,
    mut local_read: R,
    local_write: W,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let outbound = async {
        let copied = tokio::io::copy(&mut local_read, &mut send).await;
        if let Err(e) = &copied {
            tracing::debug!("lease stream: local read ended: {e}");
        }
        // Already finished or reset streams are fine here.
        let _ = send.finish();
    };
    let inbound = async move {
        let mut local_write = local_write;
        let copied = tokio::io::copy(&mut recv, &mut local_write).await;
        if let Err(e) = &copied {
            tracing::debug!("lease stream: peer read ended: {e}");
        }
        let _ = local_write.shutdown().await;
        // Shutting down a child's stdin doesn't close the pipe; dropping
        // it does, which is what tells the daemon the session is over.
        drop(local_write);
    };
    tokio::join!(outbound, inbound);
}
