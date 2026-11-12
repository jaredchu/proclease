use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const MAX_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;
pub const SCHEMA_VERSION: u32 = 1;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn duration_ms(value: &str) -> Result<u64> {
    let duration = humantime::parse_duration(value)?;
    let millis = u64::try_from(duration.as_millis())?;
    if millis == 0 || millis > MAX_LIFETIME_MS {
        bail!("duration must be greater than zero and at most 24h");
    }
    Ok(millis)
}

/// How a job's lifetime relates to the `run` invocation that launched it.
/// Records written before this field existed are detached.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// `run` returns after startup; the supervisor continues on its own.
    #[default]
    Detached,
    /// `run --foreground` holds the session until the job finishes.
    Foreground,
}

/// The supervisor control endpoint used by stop/extend and observation.
/// Records written before this field existed use a Unix-domain socket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Socket,
    /// Bounded request/reply files in the private job directory, with a
    /// process-lifetime lock proving that the serving guardian is alive.
    File,
}

impl Mode {
    /// The only transport each mode uses; there is no fallback between them.
    pub fn transport(self) -> Transport {
        match self {
            Self::Detached => Transport::Socket,
            Self::Foreground => Transport::File,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Launch {
    pub id: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub owner: String,
    pub task: Option<String>,
    pub name: Option<String>,
    pub url: Option<String>,
    pub key: Option<String>,
    pub fingerprint: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub grace_ms: u64,
    pub log_limit_bytes: u64,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub transport: Transport,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Running,
    Stopping,
    Stopped,
    Expired,
    Exited,
    Failed,
    Interrupted,
    Unknown,
}

impl State {
    pub fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Requested,
    Deadline,
    CommandExit,
    SupervisionLost,
    Signal,
    LaunchFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub schema_version: u32,
    pub id: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub owner: String,
    pub task: Option<String>,
    pub name: Option<String>,
    /// Declared by the caller; ProcLease does not imply endpoint readiness.
    pub url: Option<String>,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub state: State,
    pub reason: Option<Reason>,
    pub keeper_pid: u32,
    pub guardian_pid: Option<u32>,
    pub command_pid: Option<u32>,
    /// Opaque Linux token for the PID number space in which the keeper saved
    /// these PIDs. Observers interpret saved PIDs only from the same context;
    /// see `platform::pid_numbers_comparable`. Absent on macOS and in records
    /// written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_context: Option<String>,
    pub exit_code: Option<i32>,
    pub finished_at_ms: Option<u64>,
    /// Proof that no lifecycle writer remains and the managed group was
    /// observed absent. Set only by the keeper's final lifecycle write, after
    /// it has killed the guardian's group, reaped the guardian and accounted
    /// for capture, and only if a signal-free observation then found the group
    /// absent. Retention may collect such a terminal record without
    /// interpreting any saved PID. False when that observation found members,
    /// failed or was uncertain, in records written before the field existed,
    /// and whenever that final write did not happen. A final write alone does
    /// not set it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub finalized_by_keeper: bool,
    /// Display-only anchor for interrupted shutdown after the keeper and group
    /// are absent. The last lifecycle write is not an exact completion time.
    #[serde(skip)]
    pub last_lifecycle_write_ms: Option<u64>,
    pub log_limit_bytes: u64,
    pub logs_truncated: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub transport: Transport,
}

impl Record {
    pub fn new(launch: &Launch) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: launch.id.clone(),
            argv: launch.argv.clone(),
            cwd: launch.cwd.clone(),
            owner: launch.owner.clone(),
            task: launch.task.clone(),
            name: launch.name.clone(),
            url: launch.url.clone(),
            created_at_ms: launch.created_at_ms,
            expires_at_ms: launch.expires_at_ms,
            state: State::Starting,
            reason: None,
            keeper_pid: 0,
            guardian_pid: None,
            command_pid: None,
            pid_context: None,
            exit_code: None,
            finished_at_ms: None,
            finalized_by_keeper: false,
            last_lifecycle_write_ms: None,
            log_limit_bytes: launch.log_limit_bytes,
            logs_truncated: false,
            error: None,
            mode: launch.mode,
            transport: launch.transport,
        }
    }

    pub fn finish(&mut self) {
        self.state = match self.reason {
            Some(Reason::Requested) => State::Stopped,
            Some(Reason::Deadline) => State::Expired,
            Some(Reason::CommandExit) if self.exit_code == Some(0) => State::Exited,
            Some(Reason::CommandExit | Reason::LaunchFailed) => State::Failed,
            _ => State::Interrupted,
        };
        self.finished_at_ms = Some(now_ms());
    }

    pub fn expires_at(&self) -> String {
        humantime::format_rfc3339_millis(UNIX_EPOCH + Duration::from_millis(self.expires_at_ms))
            .to_string()
    }

    pub fn recent_history_at_ms(&self) -> Option<u64> {
        self.finished_at_ms.or(self.last_lifecycle_write_ms)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Control {
    Stop,
    Extend { by_ms: u64 },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub record: Record,
    pub error: Option<String>,
}

pub fn extend_deadline(record: &mut Record, by_ms: u64) -> Result<()> {
    if record.state != State::Running || now_ms() >= record.expires_at_ms {
        bail!("only an unexpired running lease can be extended");
    }
    if by_ms == 0 {
        bail!("extension must be greater than zero");
    }
    let deadline = record
        .expires_at_ms
        .checked_add(by_ms)
        .ok_or_else(|| anyhow::anyhow!("deadline overflow"))?;
    if deadline > record.created_at_ms.saturating_add(MAX_LIFETIME_MS) {
        bail!("total lifetime cannot exceed 24h from creation");
    }
    record.expires_at_ms = deadline;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_are_bounded_and_explicit() {
        assert_eq!(duration_ms("2h").unwrap(), 7_200_000);
        assert_eq!(duration_ms("250ms").unwrap(), 250);
        assert!(duration_ms("0s").is_err());
        assert!(duration_ms("25h").is_err());
        assert!(duration_ms("forever").is_err());
    }

    #[test]
    fn records_without_mode_remain_detached_socket_jobs() {
        let launch: Launch = serde_json::from_value(serde_json::json!({
            "id": "000000000001", "argv": ["/usr/bin/true"], "cwd": "/",
            "owner": "user", "task": null, "name": null, "url": null, "key": null,
            "fingerprint": "f", "created_at_ms": 0, "expires_at_ms": 1,
            "grace_ms": 0, "log_limit_bytes": 1
        }))
        .unwrap();
        assert_eq!(
            (launch.mode, launch.transport),
            (Mode::Detached, Transport::Socket)
        );
        let mut value = serde_json::to_value(Record::new(&launch)).unwrap();
        value.as_object_mut().unwrap().remove("mode");
        value.as_object_mut().unwrap().remove("transport");
        let record: Record = serde_json::from_value(value).unwrap();
        assert_eq!(
            (record.mode, record.transport),
            (Mode::Detached, Transport::Socket)
        );
        assert_eq!(Mode::Foreground.transport(), Transport::File);
    }
}
