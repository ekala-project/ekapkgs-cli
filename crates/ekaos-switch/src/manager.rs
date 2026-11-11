//! The running system manager, over D-Bus.
//!
//! systemd is reached on its private socket, as root's `systemctl` does,
//! so a switch works without a system bus daemon and survives the bus
//! being restarted by the switch itself. Only the logind query uses the
//! system bus.

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::oneshot;
use futures::future::{Either, select};
use zbus::message::Type as MessageType;
use zbus::proxy::CacheProperties;
use zbus::{Connection, MatchRule, MessageStream, OwnedObjectPath};

use crate::Error;
use crate::plan::UnitState;

/// How long systemd may take to reload or re-execute.
const DAEMON_RELOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// Bus traffic must be quiet this long before failed units are collected.
const SETTLE_INTERVAL: Duration = Duration::from_millis(250);
const SETTLE_MAX: Duration = Duration::from_secs(90);
const BUS_TIMEOUT: Duration = Duration::from_secs(25);
const RECONNECT_INTERVAL: Duration = Duration::from_millis(250);

const PRIVATE_SOCKET: &str = "unix:path=/run/systemd/private";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Start,
    Stop,
    Restart,
    Reload,
}

impl std::fmt::Display for JobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            JobKind::Start => "start",
            JobKind::Stop => "stop",
            JobKind::Restart => "restart",
            JobKind::Reload => "reload",
        })
    }
}

/// A job that finished with something other than success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedJob {
    pub unit: String,
    pub kind: JobKind,
    pub result: String,
}

/// A user with a running session, from logind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub uid: u32,
    pub name: String,
}

/// What the switch needs from the system manager.
pub trait Manager {
    /// Loaded units that are not inactive and not aliases.
    fn active_units(&mut self) -> Result<BTreeMap<String, UnitState>, Error>;
    fn is_active(&mut self, unit: &str) -> Result<bool, Error>;
    fn exec_main_status(&mut self, unit: &str) -> Result<i32, Error>;
    /// Queue a job in `replace` mode.
    fn submit(&mut self, kind: JobKind, unit: &str) -> Result<(), Error>;
    /// Wait for every submitted job to finish.
    fn wait_jobs(&mut self) -> Result<(), Error>;
    /// Jobs that failed, timed out or had a failed dependency.
    fn failed_jobs(&mut self) -> Vec<FailedJob>;
    fn reexecute(&mut self) -> Result<(), Error>;
    fn reload(&mut self) -> Result<(), Error>;
    fn reset_failed(&mut self) -> Result<(), Error>;
    /// Wait for job notifications to stop arriving.
    fn settle(&mut self);
    fn users(&mut self) -> Result<Vec<User>, Error>;
}

type ListedUnit = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Systemd1Manager {
    fn list_units(&self) -> zbus::Result<Vec<ListedUnit>>;
    fn get_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn restart_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn reload_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;
    fn reset_failed(&self) -> zbus::Result<()>;
    fn subscribe(&self) -> zbus::Result<()>;
    // systemd replies to these only once it is done, and not at all when
    // re-executing; completion is signalled with Reloading(false).
    #[zbus(no_reply)]
    fn reload(&self) -> zbus::Result<()>;
    #[zbus(no_reply)]
    fn reexecute(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn job_removed(
        &self,
        id: u32,
        job: OwnedObjectPath,
        unit: String,
        result: String,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    fn reloading(&self, active: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait Systemd1Unit {
    #[zbus(property)]
    fn active_state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn fragment_path(&self) -> zbus::Result<String>;
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Service",
    default_service = "org.freedesktop.systemd1"
)]
trait Systemd1Service {
    #[zbus(property)]
    fn exec_main_status(&self) -> zbus::Result<i32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    fn list_users(&self) -> zbus::Result<Vec<(u32, String, OwnedObjectPath)>>;
}

/// A future that resolves after `duration`, without depending on a
/// particular async runtime's timers.
fn sleep(duration: Duration) -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(duration);
        let _ = tx.send(());
    });
    rx
}

/// A connection to systemd's private socket, subscribed to job signals.
struct Private {
    conn: Connection,
    systemd: Systemd1ManagerProxy<'static>,
    jobs_removed: MessageStream,
    reloading: MessageStream,
}

impl Private {
    async fn connect() -> zbus::Result<Self> {
        let mut conn = zbus::conn::Builder::address(PRIVATE_SOCKET)
            .p2p()
            .method_timeout(BUS_TIMEOUT)
            .build()
            .await?;
        // Every job of a large switch is reported through these streams;
        // don't let notifications back up.
        conn.set_max_queued(NonZeroUsize::new(1 << 16).expect("non-zero"));
        let systemd = Systemd1ManagerProxy::builder(&conn)
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        // A proxy's signal streams look the sender up on the bus, which a
        // peer connection has none of, so match the signals directly.
        // The streams exist before Subscribe, so no job signal is missed.
        let jobs_removed = signal_stream(&conn, "JobRemoved").await?;
        let reloading = signal_stream(&conn, "Reloading").await?;
        systemd.subscribe().await?;
        Ok(Self {
            conn,
            systemd,
            jobs_removed,
            reloading,
        })
    }
}

async fn signal_stream(conn: &Connection, member: &'static str) -> zbus::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .path(SYSTEMD_PATH)
        .interface(SYSTEMD_MANAGER)
        .member(member)
        .build()?;
    MessageStream::for_match_rule(rule, conn, None).await
}

/// The system manager, on systemd's private socket.
pub struct DbusManager {
    bus: Private,
    pending: HashMap<OwnedObjectPath, (String, JobKind)>,
    failed: Vec<FailedJob>,
}

impl DbusManager {
    pub fn system() -> Result<Self, Error> {
        let bus = zbus::block_on(Private::connect()).map_err(Error::Dbus)?;
        Ok(Self {
            bus,
            pending: HashMap::new(),
            failed: Vec::new(),
        })
    }

    async fn unit_path(&self, unit: &str) -> zbus::Result<OwnedObjectPath> {
        self.bus.systemd.get_unit(unit).await
    }

    async fn unit_proxy(&self, path: OwnedObjectPath) -> zbus::Result<Systemd1UnitProxy<'static>> {
        Systemd1UnitProxy::builder(&self.bus.conn)
            .path(path)
            .cache_properties(CacheProperties::No)
            .build()
            .await
    }
}

impl Manager for DbusManager {
    fn active_units(&mut self) -> Result<BTreeMap<String, UnitState>, Error> {
        zbus::block_on(async {
            let mut active = BTreeMap::new();
            for (id, _, _, state, substate, following, path, ..) in
                self.bus.systemd.list_units().await?
            {
                if !following.is_empty() || state == "inactive" {
                    continue;
                }
                // Units can vanish between the two calls.
                let fragment_path = match self.unit_proxy(path).await {
                    Ok(unit) => unit.fragment_path().await.unwrap_or_default(),
                    Err(_) => String::new(),
                };
                active.insert(
                    id,
                    UnitState {
                        state,
                        substate,
                        fragment_path,
                    },
                );
            }
            Ok::<_, zbus::Error>(active)
        })
        .map_err(Error::Dbus)
    }

    fn is_active(&mut self, unit: &str) -> Result<bool, Error> {
        zbus::block_on(async {
            let path = self.unit_path(unit).await?;
            let state = self.unit_proxy(path).await?.active_state().await?;
            Ok::<_, zbus::Error>(matches!(state.as_str(), "active" | "activating"))
        })
        .map_err(Error::Dbus)
    }

    fn exec_main_status(&mut self, unit: &str) -> Result<i32, Error> {
        zbus::block_on(async {
            let path = self.unit_path(unit).await?;
            Systemd1ServiceProxy::builder(&self.bus.conn)
                .path(path)
                .cache_properties(CacheProperties::No)
                .build()
                .await?
                .exec_main_status()
                .await
        })
        .map_err(Error::Dbus)
    }

    fn submit(&mut self, kind: JobKind, unit: &str) -> Result<(), Error> {
        let job = zbus::block_on(async {
            match kind {
                JobKind::Start => self.bus.systemd.start_unit(unit, "replace").await,
                JobKind::Stop => self.bus.systemd.stop_unit(unit, "replace").await,
                JobKind::Restart => self.bus.systemd.restart_unit(unit, "replace").await,
                JobKind::Reload => self.bus.systemd.reload_unit(unit, "replace").await,
            }
        })
        .map_err(Error::Dbus)?;
        self.pending.insert(job, (unit.to_owned(), kind));
        Ok(())
    }

    fn wait_jobs(&mut self) -> Result<(), Error> {
        let pending = &mut self.pending;
        let failed = &mut self.failed;
        let stream = &mut self.bus.jobs_removed;
        zbus::block_on(async {
            while !pending.is_empty() {
                tracing::debug!("waiting for {} job(s)", pending.len());
                let Some(msg) = stream.next().await else {
                    return Err(Error::Disconnected("waiting for jobs"));
                };
                let Some(signal) = msg.ok().and_then(JobRemoved::from_message) else {
                    continue;
                };
                let Ok(args) = signal.args() else { continue };
                if let Some((unit, kind)) = pending.remove(args.job())
                    && matches!(args.result().as_str(), "timeout" | "failed" | "dependency")
                {
                    failed.push(FailedJob {
                        unit,
                        kind,
                        result: args.result().clone(),
                    });
                }
            }
            Ok(())
        })
    }

    fn failed_jobs(&mut self) -> Vec<FailedJob> {
        std::mem::take(&mut self.failed)
    }

    fn reexecute(&mut self) -> Result<(), Error> {
        let bus = &mut self.bus;
        zbus::block_on(async {
            // systemd drops its private connections when it re-executes;
            // that is the only sign it has gone, so wait for it and connect
            // to the new instance.
            bus.systemd.reexecute().await?;
            let disconnected = async { while bus.reloading.next().await.is_some() {} };
            if let Either::Right(_) =
                select(Box::pin(disconnected), sleep(DAEMON_RELOAD_TIMEOUT)).await
            {
                return Err(Error::Timeout("systemd re-execution"));
            }

            let mut waited = Duration::ZERO;
            loop {
                match Private::connect().await {
                    Ok(new) => {
                        *bus = new;
                        return Ok(());
                    },
                    Err(e) if waited >= DAEMON_RELOAD_TIMEOUT => return Err(Error::Dbus(e)),
                    Err(e) => tracing::debug!("systemd not back yet: {e}"),
                }
                let _ = sleep(RECONNECT_INTERVAL).await;
                waited += RECONNECT_INTERVAL;
            }
        })
    }

    fn reload(&mut self) -> Result<(), Error> {
        let bus = &mut self.bus;
        zbus::block_on(async {
            bus.systemd.reload().await?;
            let finished = async {
                while let Some(msg) = bus.reloading.next().await {
                    let Ok(msg) = msg else { continue };
                    if let Some(signal) = Reloading::from_message(msg)
                        && let Ok(args) = signal.args()
                        && !*args.active()
                    {
                        return true;
                    }
                }
                false
            };
            match select(Box::pin(finished), sleep(DAEMON_RELOAD_TIMEOUT)).await {
                Either::Left((true, _)) => Ok(()),
                Either::Left((false, _)) => Err(Error::Disconnected("reloading systemd")),
                Either::Right(_) => Err(Error::Timeout("systemd reload")),
            }
        })
    }

    fn reset_failed(&mut self) -> Result<(), Error> {
        zbus::block_on(self.bus.systemd.reset_failed()).map_err(Error::Dbus)
    }

    fn settle(&mut self) {
        let stream = &mut self.bus.jobs_removed;
        zbus::block_on(async {
            let mut waited = Duration::ZERO;
            while waited < SETTLE_MAX {
                match select(stream.next(), sleep(SETTLE_INTERVAL)).await {
                    Either::Left((Some(_), _)) => waited += SETTLE_INTERVAL,
                    _ => return,
                }
            }
            tracing::debug!("timed out waiting for systemd events to settle");
        });
    }

    fn users(&mut self) -> Result<Vec<User>, Error> {
        zbus::block_on(async {
            // logind is only on the system bus. Without one there is no
            // logind, so no user managers to update.
            let conn = match zbus::conn::Builder::system()
                .method_timeout(BUS_TIMEOUT)
                .build()
                .await
            {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::debug!("no system bus, skipping user managers: {e}");
                    return Ok(Vec::new());
                },
            };
            let logind = Login1ManagerProxy::new(&conn).await?;
            let users = logind.list_users().await?;
            Ok::<_, zbus::Error>(
                users
                    .into_iter()
                    .map(|(uid, name, _)| User { uid, name })
                    .collect(),
            )
        })
        .map_err(Error::Dbus)
    }
}
