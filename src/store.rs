use crate::model::{Launch, Reason, Record, State, Transport};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}

pub const HISTORY_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;
pub const HISTORY_MAX_FINISHED: usize = 100;
/// Absolute bound for one supervisor availability probe.
pub const OBSERVE_CONNECT: std::time::Duration = std::time::Duration::from_millis(500);
/// Advisory lock file guarding history collection. A bulk stop holds a shared
/// lock on its own open file description from its selection scan until its
/// results are captured; collection of job directories requires a
/// nonblocking exclusive lock. The OS releases the lock when the holder exits,
/// including on a crash or kill, so no persisted pin can be orphaned.
const RETENTION_GUARD: &str = ".retention";

#[derive(Default, Serialize)]
pub struct PruneReport {
    pub candidates: Vec<String>,
    pub removed: Vec<String>,
    pub warnings: Vec<String>,
    /// Job collection was skipped because a bulk stop protects its selection,
    /// or because protection could not be verified.
    pub deferred: bool,
}

/// Shared retention protection held for the lifetime of this value or of the
/// process, whichever ends first. It never blocks keepers or launches.
pub struct RetentionGuard {
    _file: File,
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 12
        && id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn private_directory(path: &Path) -> Result<()> {
    match DirBuilder::new().recursive(true).mode(0o700).create(path) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!(
            "state directory must be a real directory owned by the current user with mode 0700: {}",
            path.display()
        );
    }
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if file.metadata()?.len() > 1_048_576 {
        bail!("state file exceeds 1 MiB");
    }
    Ok(serde_json::from_reader(file.take(1_048_577))?)
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("state file has no parent")?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn unpublished_reservation(dir: &Path) -> Result<bool> {
    // Launch metadata is committed before any process starts. Only an empty
    // reservation or our own uncommitted atomic-write files proves no launch.
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(false);
        };
        let Some(uuid) = name.strip_prefix('.').and_then(|s| s.strip_suffix(".tmp")) else {
            return Ok(false);
        };
        let metadata = fs::symlink_metadata(entry.path())?;
        if uuid.len() != 32
            || !uuid
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Ok(false);
        }
    }
    Ok(true)
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        if !path.is_absolute() {
            bail!("state directory must be absolute");
        }
        private_directory(&path)?;
        let root = fs::canonicalize(path)?;
        if root
            .join("aaaaaaaaaaaa/ctl.sock")
            .as_os_str()
            .as_bytes()
            .len()
            > 103
        {
            bail!("state path is too long for a Unix socket; choose a shorter PROCLEASE_HOME");
        }
        Ok(Self { root })
    }
    pub fn job(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            bail!("invalid job ID; expected 12 lowercase hexadecimal characters");
        }
        let path = self.root.join(id);
        let metadata = fs::symlink_metadata(&path).with_context(|| format!("unknown job {id}"))?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            bail!("unsafe job directory");
        }
        Ok(path)
    }
    /// Confirm that a Unix control socket can be created and bound in this
    /// state directory before anything is reserved, so a host that denies
    /// Unix-domain sockets records no job and consumes no retry key. The
    /// temporary name is shorter than a job's socket path, which `open`
    /// already bounds, and is removed before returning.
    pub fn probe_control_socket(&self) -> Result<()> {
        use crate::platform::SocketStep;
        let name = format!(
            ".probe-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        let Err((step, error)) = crate::platform::probe_unix_bind(&self.root.join(name)) else {
            return Ok(());
        };
        let step = match step {
            SocketStep::Create => "create",
            SocketStep::Bind => "bind",
        };
        let hint = if crate::platform::is_denial(&error) {
            ". Access was denied, for example by a host sandbox policy or directory \
             permissions; ProcLease requires a Unix-domain control socket for every job"
        } else {
            ""
        };
        bail!(
            "cannot {step} a Unix control socket in the state directory: {error}{hint}; \
             nothing was launched and no job was recorded"
        )
    }
    pub fn reserve(&self) -> Result<String> {
        loop {
            let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
            match DirBuilder::new().mode(0o700).create(self.root.join(&id)) {
                Ok(_) => return Ok(id),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join(".lock"))?;
        FileExt::lock_exclusive(&file)?;
        Ok(file)
    }
    /// Open the guard without following a symlink, without blocking on a FIFO,
    /// and only if it is a private regular file owned by the current user.
    fn retention_file(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join(RETENTION_GUARD))
            .context("cannot open retention guard")?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            bail!("retention guard must be a private regular file owned by the current user");
        }
        Ok(file)
    }

    /// Protect every current job directory from history collection. Called
    /// with the root launch lock held, before the selection scan, so no
    /// collection can run between the snapshot and protection. Collection
    /// holds the root lock too, so a failed attempt indicates an error rather
    /// than contention; it never blocks.
    pub fn protect_retention_locked(&self) -> Result<RetentionGuard> {
        let file = self.retention_file()?;
        FileExt::try_lock_shared(&file)
            .context("cannot protect selected jobs from history collection")?;
        Ok(RetentionGuard { _file: file })
    }

    /// An exclusive guard permitting job collection, or `None` while any
    /// bulk stop holds protection. Never blocks.
    fn collection_permit(&self) -> Result<Option<File>> {
        let file = self.retention_file()?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(file)),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                Ok(None)
            }
            Err(error) => Err(error).context("cannot check retention guard"),
        }
    }

    pub fn prune(&self, dry_run: bool, completed_id: Option<&str>) -> Result<PruneReport> {
        let _lock = self.lock()?;
        self.prune_locked(dry_run, completed_id)
    }

    /// The root launch lock serializes collection with retry lookup/reservation.
    /// Quarantined directories are always retried; they left the job namespace
    /// before any bulk selection could protect them. Job directories are only
    /// collected while no bulk stop holds retention protection; otherwise the
    /// report is marked deferred and a later prune collects them.
    pub fn prune_locked(&self, dry_run: bool, completed_id: Option<&str>) -> Result<PruneReport> {
        let mut report = PruneReport::default();
        // Held until return. Protection is acquired only under the root lock,
        // which this caller holds, so it cannot begin during collection.
        let _permit = match self.collection_permit() {
            Ok(Some(permit)) => Some(permit),
            Ok(None) => {
                report.deferred = true;
                None
            }
            Err(error) => {
                report.deferred = true;
                report
                    .warnings
                    .push(format!("history collection deferred: {error:#}"));
                None
            }
        };
        let mut finished = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = quarantined_id(&name) {
                // Only the reserved quarantine namespace is retried. Jobs enter
                // it atomically after verification, before recursive deletion.
                let result = (|| -> Result<()> {
                    let metadata = fs::symlink_metadata(entry.path())?;
                    if !metadata.is_dir()
                        || metadata.uid() != unsafe { libc::geteuid() }
                        || metadata.mode() & 0o077 != 0
                    {
                        bail!("unsafe history quarantine directory");
                    }
                    report.candidates.push(id.to_owned());
                    if !dry_run {
                        fs::remove_dir_all(entry.path())?;
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) if !dry_run => report.removed.push(id.to_owned()),
                    Ok(()) => (),
                    Err(error) => report
                        .warnings
                        .push(format!("{id}: cleanup deferred: {error:#}")),
                }
                continue;
            }
            if !valid_id(&name) {
                continue;
            }
            let result = (|| -> Result<Option<(u64, String)>> {
                let dir = self.job(&name)?;
                if unpublished_reservation(&dir)? {
                    return Ok(None);
                }
                let record = self.record(&name)?;
                if record.id != name || record.state.active() || record.state == State::Unknown {
                    return Ok(None);
                }
                let Some(time) = record.finished_at_ms else {
                    return Ok(None);
                };
                // The marker is set by the keeper's final write only after
                // group KILL, guardian reaping and an observation that found
                // the group absent, so no supervised process or lifecycle
                // writer remains. No saved PID is interpreted, so this holds
                // after a reboot or from another PID context.
                if record.finalized_by_keeper {
                    return Ok(Some((time, name.clone())));
                }
                // Unfinalized terminal records (earlier versions, a keeper
                // that did not complete its final write, or one that could not
                // observe its group absent) need verified absence.
                let own_completion =
                    completed_id == Some(name.as_str()) && record.keeper_pid == std::process::id();
                // Absence of saved PIDs is meaningful only in their own PID
                // space; otherwise the record is preserved. A keeper finishing
                // its own job saved these PIDs itself.
                let comparable = own_completion
                    || crate::platform::pid_numbers_comparable(record.pid_context.as_deref());
                if !own_completion
                    && !never_supervised(&record)
                    && !(comparable && process_is_absent(record.keeper_pid))
                {
                    return Ok(None);
                }
                if record
                    .guardian_pid
                    .is_some_and(|pid| pid <= 1 || !comparable || !group_is_absent(pid))
                {
                    return Ok(None);
                }
                Ok(Some((time, name.clone())))
            })();
            match result {
                Ok(Some(job)) => finished.push(job),
                Ok(None) => (),
                Err(error) => report
                    .warnings
                    .push(format!("{name}: history preserved: {error:#}")),
            }
        }
        finished.sort();
        if report.deferred && !dry_run {
            return Ok(report);
        }
        let excess = finished.len().saturating_sub(HISTORY_MAX_FINISHED);
        let now = crate::model::now_ms();
        for (index, (time, id)) in finished.into_iter().enumerate() {
            if index >= excess && now.saturating_sub(time) < HISTORY_MAX_AGE_MS {
                continue;
            }
            report.candidates.push(id.clone());
            if dry_run {
                continue;
            }
            let result = (|| -> Result<()> {
                let quarantine = self
                    .root
                    .join(format!(".prune-{id}-{}", uuid::Uuid::new_v4().simple()));
                // Removing the job from the retry namespace first prevents a
                // partial deletion from poisoning every future keyed launch.
                fs::rename(self.job(&id)?, &quarantine)?;
                fs::remove_dir_all(quarantine)?;
                Ok(())
            })();
            match result {
                Ok(()) => report.removed.push(id),
                Err(error) => report
                    .warnings
                    .push(format!("{id}: cleanup deferred: {error:#}")),
            }
        }
        Ok(report)
    }

    pub fn record(&self, id: &str) -> Result<Record> {
        let mut record: Record = read_json(&self.job(id)?.join("record.json"))?;
        // A record is addressed only by its directory. A record naming another
        // job must never redirect selection, observation or control to it.
        if record.id != id {
            bail!("record names a different job ID; refusing to use it");
        }
        if record.state != State::Starting {
            record.logs_truncated |= self.capture_truncated(id)?;
        }
        Ok(record)
    }
    pub fn capture_truncated(&self, id: &str) -> Result<bool> {
        let path = self.job(id)?.join("capture.status");
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let mut value = [0u8; 1];
        file.read_exact(&mut value)?;
        match value[0] {
            b'0' => Ok(false),
            b'1' => Ok(true),
            _ => bail!("invalid capture status"),
        }
    }
    pub fn launch(&self, id: &str) -> Result<Launch> {
        read_json(&self.job(id)?.join("launch.json"))
    }
    pub fn save(&self, record: &Record) -> Result<()> {
        atomic_json(&self.job(&record.id)?.join("record.json"), record)
    }
    /// Called under the launch lock. Unpublished reservations cannot have
    /// launched a process; ambiguous unreadable metadata still fails closed.
    pub fn find_retry(&self, key: &str) -> Result<Option<(Launch, Record)>> {
        let mut found = None;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().to_string();
            if !valid_id(&id) {
                continue;
            }
            let dir = self.job(&id)?;
            if unpublished_reservation(&dir)? {
                continue;
            }
            let launch = self
                .launch(&id)
                .with_context(|| format!("cannot safely resolve retry key: {id}"))?;
            if launch.key.as_deref() == Some(key) {
                if found.is_some() {
                    bail!("retry key belongs to multiple jobs; inspect state before retrying");
                }
                found = Some((launch, self.observed_record(&id)?));
            }
        }
        Ok(found)
    }
    pub fn observed_record(&self, id: &str) -> Result<Record> {
        self.observed_record_until(id, std::time::Instant::now() + OBSERVE_CONNECT)
    }
    /// `observed_record` with a caller-supplied absolute probe deadline, so a
    /// bounded caller (such as a readiness wait) can keep its overall budget.
    pub fn observed_record_until(&self, id: &str, probe: std::time::Instant) -> Result<Record> {
        let mut record = self.record(id)?;
        if !record.state.active() {
            return Ok(record);
        }
        // Observation only: never signal an OS PID copied from disk.
        // The probe connect is bounded; scans hold the root lock while probing.
        // A file endpoint is live only while its guardian holds the control
        // lock; that check never blocks. Each job uses only its own transport.
        let endpoint = match record.transport {
            Transport::Socket => {
                crate::platform::connect_unix(&self.job(id)?.join("ctl.sock"), probe).map(drop)
            }
            Transport::File => crate::filectl::endpoint_live(&self.job(id)?),
        };
        if let Err(error) = endpoint {
            // Saved PIDs are probed only from the PID space they were saved in.
            let comparable = crate::platform::pid_numbers_comparable(record.pid_context.as_deref());
            let unavailable = || {
                let mut message = format!(
                    "control endpoint unavailable ({}); cleanup is not verified",
                    error.kind()
                );
                if !comparable {
                    message.push_str(
                        "; its saved PIDs belong to another or unverifiable PID context \
                         (for example a sandbox that runs each tool call in its own PID \
                         namespace) and are not probed from here",
                    );
                }
                message
            };
            if record.state == State::Stopping && !comparable {
                // Absence cannot be checked, so shutdown is never inferred.
                // A healthy supervisor closes its endpoint shortly before the
                // keeper writes the final record, so `stopping` is kept for a
                // bounded window after the last lifecycle write; then, or if
                // that write time cannot be established, it is `unknown`.
                let message = unavailable();
                if self.within_finalization_window(id) {
                    record.error = Some(match record.error.take() {
                        Some(previous) => {
                            format!("{previous}; {message}; awaiting the supervisor's final record")
                        }
                        None => format!("{message}; awaiting the supervisor's final record"),
                    });
                } else {
                    record.state = State::Unknown;
                    record.error = Some(message);
                }
            } else if record.state == State::Stopping {
                if record.guardian_pid.is_some_and(group_is_absent) {
                    record.finish();
                    // The keeper died before recording completion; the exact
                    // finish time is unknown, even though absence is observed.
                    record.finished_at_ms = None;
                    if process_is_absent(record.keeper_pid) {
                        // No lifecycle writer remains. Reuse the persisted
                        // shutdown write as a stable display anchor, including
                        // for records left by older versions. Never rewrite
                        // lifecycle state or invent an exact completion time.
                        record.last_lifecycle_write_ms = self
                            .record_written_at(id)
                            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                            .and_then(|age| u64::try_from(age.as_millis()).ok())
                            .filter(|&time| time <= crate::model::now_ms());
                    }
                }
            } else if record.state != State::Starting
                || crate::model::now_ms().saturating_sub(record.created_at_ms) > 5_000
            {
                record.state = State::Unknown;
                record.error = Some(unavailable());
            }
        }
        Ok(record)
    }
    /// Modification time of a job's regular `record.json`, the time of its
    /// last lifecycle write.
    fn record_written_at(&self, id: &str) -> Option<std::time::SystemTime> {
        fs::symlink_metadata(self.job(id).ok()?.join("record.json"))
            .ok()
            .filter(|metadata| metadata.is_file())
            .and_then(|metadata| metadata.modified().ok())
    }
    /// Whether the last lifecycle write is recent enough that a healthy
    /// supervisor may still be finalizing: within the stop bound for this
    /// job's grace (capped at `MAX_GRACE_MS`, plus 2s). Unreadable launch
    /// metadata and a missing, unreadable or future write time fail closed.
    fn within_finalization_window(&self, id: &str) -> bool {
        let Ok(launch) = self.launch(id) else {
            return false;
        };
        let Some(written) = self.record_written_at(id) else {
            return false;
        };
        std::time::SystemTime::now()
            .duration_since(written)
            .is_ok_and(|age| age <= crate::lifecycle::stop_bound(launch.grace_ms))
    }
    pub fn list(&self) -> Result<(Vec<Record>, Vec<String>)> {
        let mut jobs = Vec::new();
        let mut warnings = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().to_string();
            if !valid_id(&id) {
                continue;
            }
            if self
                .job(&id)
                .is_ok_and(|dir| unpublished_reservation(&dir).unwrap_or(false))
            {
                continue;
            }
            match self.observed_record(&id) {
                Ok(record) => jobs.push(record),
                Err(error) => warnings.push(format!("{id}: {error}")),
            }
        }
        jobs.sort_by_key(|job| job.created_at_ms);
        Ok((jobs, warnings))
    }
}

/// A launch finalized by the CLI after its keeper could not be spawned. Every
/// keeper records its PID before any other lifecycle write, so a failed record
/// with no keeper, guardian or command PID was never supervised and has no
/// process to verify. Starting, unknown and other terminal states never match.
fn never_supervised(record: &Record) -> bool {
    record.state == State::Failed
        && record.reason == Some(Reason::LaunchFailed)
        && record.keeper_pid == 0
        && record.guardian_pid.is_none()
        && record.command_pid.is_none()
}

fn group_is_absent(pid: u32) -> bool {
    matches!(crate::platform::group_has_live_members(pid), Ok(false))
}

fn process_is_absent(pid: u32) -> bool {
    pid > 1
        && unsafe { libc::kill(pid as i32, 0) } == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn quarantined_id(name: &str) -> Option<&str> {
    let (id, uuid) = name.strip_prefix(".prune-")?.split_once('-')?;
    (valid_id(id)
        && uuid.len() == 32
        && uuid
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
    .then_some(id)
}
