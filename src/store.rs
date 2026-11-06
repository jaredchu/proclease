use crate::model::{Launch, Record, State};
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

#[derive(Default, Serialize)]
pub struct PruneReport {
    pub candidates: Vec<String>,
    pub removed: Vec<String>,
    pub warnings: Vec<String>,
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
    pub fn prune(&self, dry_run: bool, completed_id: Option<&str>) -> Result<PruneReport> {
        let _lock = self.lock()?;
        self.prune_locked(dry_run, completed_id)
    }

    /// The root launch lock serializes collection with retry lookup/reservation.
    pub fn prune_locked(&self, dry_run: bool, completed_id: Option<&str>) -> Result<PruneReport> {
        let mut report = PruneReport::default();
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
                let own_completion =
                    completed_id == Some(name.as_str()) && record.keeper_pid == std::process::id();
                if !own_completion && !process_is_absent(record.keeper_pid) {
                    return Ok(None);
                }
                if record
                    .guardian_pid
                    .is_some_and(|pid| pid <= 1 || !group_is_absent(pid))
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
        let mut record = self.record(id)?;
        // Observation only: never signal an OS PID copied from disk.
        if record.state.active()
            && std::os::unix::net::UnixStream::connect(self.job(id)?.join("ctl.sock")).is_err()
        {
            if record.state == State::Stopping {
                if record.guardian_pid.is_some_and(group_is_absent) {
                    record.finish();
                    // The keeper died before recording completion; the exact
                    // finish time is unknown, even though absence is observed.
                    record.finished_at_ms = None;
                }
            } else if record.state != State::Starting
                || crate::model::now_ms().saturating_sub(record.created_at_ms) > 5_000
            {
                record.state = State::Unknown;
                record.error = Some("control endpoint unavailable; cleanup is not verified".into());
            }
        }
        Ok(record)
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
