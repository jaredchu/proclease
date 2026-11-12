//! Optional readiness observation for `run`.
//!
//! Readiness is checked by the foreground CLI after launch, outside the root
//! launch lock and outside the job's lifecycle. It never changes, renews or
//! extends a lease, never relaunches a job, never sends a control request and
//! never persists anything. A timeout leaves the job supervised under its
//! original deadline. A successful loopback TCP connection cannot prove which
//! process owns the port or that an application endpoint is healthy.

use crate::{
    model::{now_ms, Record, State},
    store::{Store, OBSERVE_CONNECT},
};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(300);
pub const MAX_LOG_TEXT_BYTES: usize = 512;
/// Interval between readiness polls.
const POLL: Duration = Duration::from_millis(50);
/// Minimum interval between supervisor availability probes.
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// Upper bound for one loopback TCP connection attempt.
const CONNECT_ATTEMPT: Duration = Duration::from_millis(250);
const READ_CHUNK: usize = 64 * 1024;
/// Bytes of captured output examined per poll, so lifecycle checks and the
/// overall budget are revisited between bounded reads.
const READ_PER_POLL: u64 = 1_048_576;
/// Same bound as the largest supported capture limit (`--log-limit`).
const MAX_LOG_BYTES: u64 = 16 * 1_048_576;

/// Invocation-time readiness criteria. They are not part of a job's launch
/// identity, so a keyed retry can check a retained job with other criteria.
pub struct Spec {
    pub port: Option<u16>,
    pub log: Option<String>,
    pub timeout: Duration,
}

/// Validate readiness flags before anything is launched.
pub fn spec(port: Option<u16>, log: Option<&str>, timeout: Option<&str>) -> Result<Option<Spec>> {
    if port.is_none() && log.is_none() {
        if timeout.is_some() {
            bail!("--wait-timeout requires --wait-for-port or --wait-for-log");
        }
        return Ok(None);
    }
    if port == Some(0) {
        bail!("--wait-for-port must be between 1 and 65535");
    }
    if let Some(text) = log {
        if text.is_empty() || text.len() > MAX_LOG_TEXT_BYTES || text.chars().any(char::is_control)
        {
            bail!("--wait-for-log text must be 1 to 512 bytes without control characters");
        }
    }
    let timeout = match timeout {
        None => DEFAULT_TIMEOUT,
        Some(value) => {
            let parsed = humantime::parse_duration(value)
                .map_err(|error| anyhow::anyhow!("invalid --wait-timeout: {error}"))?;
            if parsed.as_millis() == 0 || parsed > MAX_TIMEOUT {
                bail!("--wait-timeout must be at least 1ms and at most 5m");
            }
            parsed
        }
    };
    Ok(Some(Spec {
        port,
        log: log.map(str::to_owned),
        timeout,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    TimedOut,
    LeaseExpired,
    JobFinished,
    JobStopping,
    SupervisionUnavailable,
    ObservationFailed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::TimedOut => "timed_out",
            Self::LeaseExpired => "lease_expired",
            Self::JobFinished => "job_finished",
            Self::JobStopping => "job_stopping",
            Self::SupervisionUnavailable => "supervision_unavailable",
            Self::ObservationFailed => "observation_failed",
        }
    }
}

pub struct Outcome {
    pub status: Status,
    /// The latest observed lifecycle record.
    pub record: Record,
    pub port_address: Option<SocketAddr>,
    pub log_seen: bool,
    pub waited: Duration,
    pub error: Option<String>,
}

impl Outcome {
    pub fn ready(&self) -> bool {
        self.status == Status::Ready
    }

    pub fn view(&self, spec: &Spec) -> Value {
        json!({
            "status": self.status.as_str(),
            "ready": self.ready(),
            "timeout_ms": spec.timeout.as_millis() as u64,
            "waited_ms": self.waited.as_millis() as u64,
            "port": spec.port.map(|port| json!({
                "port": port,
                "satisfied": self.port_address.is_some(),
                "address": self.port_address.map(|address| address.to_string()),
            })),
            "log": spec.log.as_ref().map(|text| json!({
                "text": text,
                "satisfied": self.log_seen,
                "logs_truncated": self.record.logs_truncated,
            })),
            "error": self.error,
        })
    }
}

/// Streaming literal byte search over captured output. Keeps only the last
/// `needle.len() - 1` bytes between reads, so a match split across read or
/// capture chunk boundaries is found with bounded memory.
pub struct LogScanner {
    needle: Vec<u8>,
    carry: Vec<u8>,
    offset: u64,
    found: bool,
}

impl LogScanner {
    pub fn new(text: &str) -> Self {
        Self {
            needle: text.as_bytes().to_vec(),
            carry: Vec::new(),
            offset: 0,
            found: false,
        }
    }

    pub fn found(&self) -> bool {
        self.found
    }

    pub fn feed(&mut self, chunk: &[u8]) -> bool {
        if self.found || self.needle.is_empty() {
            self.found = true;
            return true;
        }
        let mut window = std::mem::take(&mut self.carry);
        window.extend_from_slice(chunk);
        if window
            .windows(self.needle.len())
            .any(|candidate| candidate == self.needle.as_slice())
        {
            self.found = true;
            return true;
        }
        let keep = (self.needle.len() - 1).min(window.len());
        self.carry = window.split_off(window.len() - keep);
        false
    }

    /// Read newly captured bytes, at most `READ_PER_POLL` per call, stopping
    /// early once `deadline` passes. The capture file only grows (up to its
    /// byte cap), so an offset suffices. The file is opened without following
    /// a symlink and without blocking on a FIFO, and only a regular file is
    /// read, so a damaged capture path cannot stall the bounded wait.
    fn poll(&mut self, path: &Path, deadline: Instant) -> Result<()> {
        if self.found || self.offset >= MAX_LOG_BYTES || Instant::now() >= deadline {
            return Ok(());
        }
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if !file.metadata()?.is_file() {
            bail!("captured output is not a regular file");
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let budget = READ_PER_POLL.min(MAX_LOG_BYTES - self.offset);
        let mut reader = file.take(budget);
        let mut buffer = vec![0u8; READ_CHUNK];
        loop {
            // Local reads cannot be interrupted, but the budget is revisited
            // between bounded chunks.
            if Instant::now() >= deadline {
                return Ok(());
            }
            let size = match reader.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(size) => size,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            self.offset += size as u64;
            if self.feed(&buffer[..size]) {
                return Ok(());
            }
        }
    }
}

/// Attempt one TCP connection to the loopback port, IPv4 then IPv6, within
/// the remaining budget. Only literal loopback addresses are used; no name is
/// resolved. The connection is closed immediately without sending data.
fn loopback_accepts(port: u16, deadline: Instant) -> Option<SocketAddr> {
    for ip in [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ] {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let address = SocketAddr::new(ip, port);
        if TcpStream::connect_timeout(&address, remaining.min(CONNECT_ATTEMPT)).is_ok() {
            return Some(address);
        }
    }
    None
}

fn lifecycle_stop(record: &Record) -> Option<Status> {
    match record.state {
        State::Starting | State::Running => {
            (now_ms() >= record.expires_at_ms).then_some(Status::LeaseExpired)
        }
        State::Stopping => Some(Status::JobStopping),
        State::Unknown => Some(Status::SupervisionUnavailable),
        _ => Some(Status::JobFinished),
    }
}

/// Wait for every requested criterion while the job is running, within one
/// monotonic budget that starts now. Every connection attempt and supervisor
/// probe is bounded by that budget; captured output reads are bounded in size
/// and revisit the budget between chunks. Ready is reported only if the budget
/// has not run out, the requested port accepts again after the log criterion
/// is met, and the job is still running under its unexpired lease. A terminal
/// or unverified record is never reported ready, even if its retained output
/// or a reused port matches.
pub fn wait(store: &Store, id: &str, spec: &Spec, initial: Record) -> Outcome {
    let started = Instant::now();
    wait_until(store, id, spec, initial, started, started + spec.timeout)
}

fn wait_until(
    store: &Store,
    id: &str,
    spec: &Spec,
    initial: Record,
    started: Instant,
    deadline: Instant,
) -> Outcome {
    let mut scanner = spec.log.as_deref().map(LogScanner::new);
    let mut port_address = None;
    let mut record = initial;
    let mut last_probe: Option<Instant> = None;
    let spent = || Instant::now() >= deadline;
    let finish = |status: Status,
                  record: Record,
                  port_address: Option<SocketAddr>,
                  scanner: &Option<LogScanner>,
                  error: Option<String>| Outcome {
        status,
        record,
        port_address,
        log_seen: scanner.as_ref().is_some_and(LogScanner::found),
        waited: started.elapsed(),
        error,
    };
    let log_path = match store.job(id) {
        Ok(dir) => dir.join("output.log"),
        Err(error) => {
            let message = format!("{error:#}");
            return finish(
                Status::ObservationFailed,
                record,
                None,
                &scanner,
                Some(message),
            );
        }
    };
    // Observe lifecycle state. A supervisor probe cut short by the overall
    // budget is a timeout, not evidence that supervision is unavailable.
    let observe = |probe_deadline: Instant| -> Result<(Record, bool)> {
        let observed = store.observed_record_until(id, probe_deadline.min(deadline))?;
        if observed.state == State::Unknown && spent() {
            return Ok((store.record(id)?, true));
        }
        Ok((observed, false))
    };
    loop {
        let now = Instant::now();
        let observed = if last_probe.is_none_or(|time| now.duration_since(time) >= PROBE_INTERVAL) {
            last_probe = Some(now);
            observe(now + OBSERVE_CONNECT)
        } else {
            store.record(id).map(|record| (record, false))
        };
        match observed {
            Ok((current, budget_spent)) => {
                record = current;
                if budget_spent {
                    return finish(Status::TimedOut, record, port_address, &scanner, None);
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                return finish(
                    Status::ObservationFailed,
                    record,
                    port_address,
                    &scanner,
                    Some(message),
                );
            }
        }
        if let Some(status) = lifecycle_stop(&record) {
            return finish(status, record, port_address, &scanner, None);
        }
        // Start no criteria work once the budget is spent.
        if spent() {
            return finish(Status::TimedOut, record, port_address, &scanner, None);
        }
        if record.state == State::Running {
            if let Some(scanner) = scanner.as_mut() {
                if let Err(error) = scanner.poll(&log_path, deadline) {
                    let message = format!("cannot read captured output: {error:#}");
                    let found = scanner.found();
                    let mut outcome = finish(
                        Status::ObservationFailed,
                        record,
                        port_address,
                        &None,
                        Some(message),
                    );
                    outcome.log_seen = found;
                    return outcome;
                }
            }
            if let Some(port) = spec.port {
                if port_address.is_none() && !spent() {
                    port_address = loopback_accepts(port, deadline);
                }
            }
            let met = (spec.port.is_none() || port_address.is_some())
                && scanner.as_ref().is_none_or(LogScanner::found);
            if met && !spent() {
                // Criteria are satisfied; confirm the job is still running
                // under supervision, then confirm the port still accepts, so
                // an earlier connection cannot satisfy a later log match after
                // the listener has gone away.
                last_probe = Some(Instant::now());
                match observe(Instant::now() + OBSERVE_CONNECT) {
                    Ok((current, budget_spent)) => {
                        record = current;
                        if budget_spent {
                            return finish(Status::TimedOut, record, port_address, &scanner, None);
                        }
                        if let Some(status) = lifecycle_stop(&record) {
                            return finish(status, record, port_address, &scanner, None);
                        }
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        return finish(
                            Status::ObservationFailed,
                            record,
                            port_address,
                            &scanner,
                            Some(message),
                        );
                    }
                }
                if let Some(port) = spec.port {
                    port_address = if spent() {
                        None
                    } else {
                        loopback_accepts(port, deadline)
                    };
                }
                // The budget is checked unconditionally before success.
                if spent() {
                    return finish(Status::TimedOut, record, port_address, &scanner, None);
                }
                if (spec.port.is_none() || port_address.is_some())
                    && record.state == State::Running
                    && now_ms() < record.expires_at_ms
                {
                    return finish(Status::Ready, record, port_address, &scanner, None);
                }
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return finish(Status::TimedOut, record, port_address, &scanner, None);
        }
        thread::sleep(POLL.min(remaining));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Launch;
    use std::{os::unix::net::UnixListener, sync::mpsc};

    fn store() -> (tempfile::TempDir, Store) {
        let mut builder = tempfile::Builder::new();
        builder.prefix("r");
        let dir = match std::env::var_os("PROCLEASE_TEST_ROOT") {
            Some(root) => {
                std::fs::create_dir_all(&root).unwrap();
                builder.tempdir_in(root).unwrap()
            }
            None => builder.tempdir().unwrap(),
        };
        let store = Store::open(dir.path().join("s")).unwrap();
        (dir, store)
    }

    /// A running record whose control endpoint accepts connections, so every
    /// supervisor probe succeeds immediately. No process is started.
    fn running_job(store: &Store) -> (Record, UnixListener) {
        let id = store.reserve().unwrap();
        let now = now_ms();
        let launch = Launch {
            id: id.clone(),
            argv: vec!["/usr/bin/true".into()],
            cwd: "/".into(),
            owner: "o".into(),
            task: None,
            name: None,
            url: None,
            key: None,
            fingerprint: String::new(),
            created_at_ms: now,
            expires_at_ms: now + 60_000,
            grace_ms: 100,
            log_limit_bytes: 1024,
            mode: Default::default(),
            transport: Default::default(),
        };
        let mut record = Record::new(&launch);
        record.state = State::Running;
        store.save(&record).unwrap();
        let listener = UnixListener::bind(store.job(&id).unwrap().join("ctl.sock")).unwrap();
        (record, listener)
    }

    fn log_spec(text: &str) -> Spec {
        Spec {
            port: None,
            log: Some(text.into()),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn spent_budget_never_reports_ready_even_when_probes_succeed() {
        let (_dir, store) = store();
        let (record, _listener) = running_job(&store);
        let path = store.job(&record.id).unwrap().join("output.log");
        std::fs::write(&path, b"booting\nREADY-TOKEN\n").unwrap();
        let spec = log_spec("READY-TOKEN");
        // An already exhausted budget: the connect and the match would both
        // succeed, but no success may be reported after the deadline.
        let started = Instant::now();
        let outcome = wait_until(&store, &record.id, &spec, record.clone(), started, started);
        assert_eq!(outcome.status, Status::TimedOut);
        assert!(!outcome.log_seen, "no scan may start after the deadline");
        assert_eq!(outcome.record.id, record.id);
        assert_eq!(outcome.record.state, State::Running);
        assert_eq!(outcome.record.expires_at_ms, record.expires_at_ms);
        // The scanner itself reads nothing once the budget is spent.
        let mut scanner = LogScanner::new("READY-TOKEN");
        scanner.poll(&path, started).unwrap();
        assert_eq!(scanner.offset, 0);
        assert!(!scanner.found());
        // The same job and output with budget left is ready, so the timeout
        // above is caused by the deadline alone.
        let started = Instant::now();
        let outcome = wait_until(
            &store,
            &record.id,
            &spec,
            record.clone(),
            started,
            started + spec.timeout,
        );
        assert_eq!(outcome.status, Status::Ready);
        assert!(outcome.log_seen);
        assert_eq!(
            store.record(&record.id).unwrap().expires_at_ms,
            record.expires_at_ms
        );
    }

    #[test]
    fn non_regular_capture_fails_observation_without_blocking() {
        let (_dir, store) = store();
        let (record, _listener) = running_job(&store);
        let path = store.job(&record.id).unwrap().join("output.log");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = mpsc::channel();
        let worker = {
            let store = Store::open(store.root.clone()).unwrap();
            let record = record.clone();
            thread::spawn(move || {
                let spec = log_spec("READY-TOKEN");
                let _ = sender.send(wait(&store, &record.id, &spec, record.clone()));
            })
        };
        let outcome = match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(outcome) => outcome,
            Err(_) => {
                // Bounded teardown: a reader blocked in open() is released by
                // a read-write open, which never blocks on a FIFO.
                let _release = OpenOptions::new().read(true).write(true).open(&path);
                let _ = worker.join();
                panic!("readiness wait blocked on a FIFO capture path");
            }
        };
        worker.join().unwrap();
        assert_eq!(outcome.status, Status::ObservationFailed);
        let error = outcome.error.unwrap();
        assert!(error.contains("not a regular file"), "{error}");
        assert_eq!(outcome.record.id, record.id);
        assert_eq!(outcome.record.state, State::Running);
        // Observation changed nothing in the lifecycle record.
        let current = store.record(&record.id).unwrap();
        assert_eq!(current.state, State::Running);
        assert_eq!(current.expires_at_ms, record.expires_at_ms);
    }

    #[test]
    fn literal_matches_survive_every_chunk_boundary() {
        let needle = "ready on http://[::1]:5173/";
        let text = format!("{}{needle}{}", "x".repeat(37), "y".repeat(11));
        let bytes = text.as_bytes();
        for split in 0..=bytes.len() {
            let mut scanner = LogScanner::new(needle);
            let first = scanner.feed(&bytes[..split]);
            let second = scanner.feed(&bytes[split..]);
            assert!(first || second, "missed match split at {split}");
        }
        // Byte-at-a-time delivery keeps only the bounded carry.
        let mut scanner = LogScanner::new(needle);
        let mut seen = false;
        for byte in bytes {
            seen |= scanner.feed(std::slice::from_ref(byte));
            assert!(scanner.carry.len() < needle.len());
        }
        assert!(seen);
    }

    #[test]
    fn text_is_literal_not_a_pattern() {
        let mut scanner = LogScanner::new("a.c");
        assert!(!scanner.feed(b"abc a-c aXc"));
        assert!(scanner.feed(b" a.c"));
        let mut scanner = LogScanner::new("$(echo hi) *");
        assert!(!scanner.feed(b"hi"));
        assert!(scanner.feed(b"$(echo hi) *"));
    }

    #[test]
    fn readiness_flags_are_validated_before_launch() {
        assert!(spec(None, None, None).unwrap().is_none());
        assert!(spec(None, None, Some("5s")).is_err());
        assert!(spec(Some(0), None, None).is_err());
        assert!(spec(None, Some(""), None).is_err());
        assert!(spec(None, Some("a\nb"), None).is_err());
        assert!(spec(None, Some(&"x".repeat(513)), None).is_err());
        assert!(spec(Some(3000), None, Some("0s")).is_err());
        assert!(spec(Some(3000), None, Some("301s")).is_err());
        assert!(spec(Some(3000), None, Some("soon")).is_err());
        let both = spec(Some(3000), Some("ready"), None).unwrap().unwrap();
        assert_eq!(both.timeout, DEFAULT_TIMEOUT);
        let max = spec(None, Some(&"x".repeat(512)), Some("5m"))
            .unwrap()
            .unwrap();
        assert_eq!(max.timeout, MAX_TIMEOUT);
    }
}
