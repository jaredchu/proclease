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
    pub exit_code: Option<i32>,
    pub finished_at_ms: Option<u64>,
    /// Display-only anchor for interrupted shutdown after the keeper and group
    /// are absent. The last lifecycle write is not an exact completion time.
    #[serde(skip)]
    pub last_lifecycle_write_ms: Option<u64>,
    pub log_limit_bytes: u64,
    pub logs_truncated: bool,
    pub error: Option<String>,
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
            exit_code: None,
            finished_at_ms: None,
            last_lifecycle_write_ms: None,
            log_limit_bytes: launch.log_limit_bytes,
            logs_truncated: false,
            error: None,
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

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Control {
    Stop,
    Extend { by_ms: u64 },
}

#[derive(Serialize, Deserialize)]
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
}
