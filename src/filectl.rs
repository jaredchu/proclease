//! File-based supervisor control for foreground jobs.
//!
//! A foreground job's guardian holds an exclusive advisory lock on
//! `ctl.lock` for its whole lifetime; the OS releases it when the guardian
//! exits, however it exits. Observers prove a live endpoint by failing to take
//! a shared lock, never from a heartbeat, timestamp or persisted state.
//!
//! Requests are single files `ctl.d/req-<id>` with an unpredictable 32-digit
//! ID, published by atomic rename. The guardian claims a request by unlinking
//! it before applying it; a client withdraws an untaken request the same way,
//! so exactly one side wins. The reply is published as `ctl.d/rep-<id>`. Only
//! `stop` and `extend` exist.
//!
//! A request is valid only inside its own window: from 1 second before its
//! creation time until the 2-second exchange budget after it. The guardian
//! remembers every applied ID until that window has closed, so a re-published
//! copy of an applied request is never applied again; afterwards the copy is
//! stale. When the bounded memory is full of IDs whose windows are still open,
//! new requests are refused rather than forgetting an ID early.
//!
//! This is private same-user state, protected by directory ownership and
//! mode. It does not authenticate an agent or caller: any same-user process
//! can write an entirely new request, so the deduplication protects
//! cooperative clients from accidental double application, not against a
//! hostile process with the user's privileges.

use crate::model::{now_ms, Control, Record, Reply};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

/// Private request/reply directory inside a job directory.
pub const CONTROL_DIR: &str = "ctl.d";
/// Lock file held exclusively by a live guardian.
pub const CONTROL_LOCK: &str = "ctl.lock";
/// Largest accepted request, matching the socket transport.
pub const REQUEST_LIMIT: u64 = 4096;
/// A client refuses to add a request when this many entries are already in
/// the control directory. Bounds abandoned-client disk growth. The count is
/// a per-client check, not an atomic global admission: clients that count at
/// the same moment can each add one request beyond it. The guardian's scan
/// and batch limits stay bounded regardless.
const QUEUE_LIMIT: usize = 64;
/// Directory entries examined by the guardian per tick.
const SCAN_LIMIT: usize = 128;
/// Requests claimed by the guardian per tick, matching the socket batch.
const BATCH_LIMIT: usize = 8;
/// Age after which unclaimed replies and partial writes are removed.
const ABANDONED_AFTER: Duration = Duration::from_secs(10);
/// Applied request IDs whose validity windows are still open. When it is
/// full, further requests are refused, never evicting a replayable ID.
const SEEN_LIMIT: usize = 256;
/// How far a request's creation time may be ahead of the guardian's clock.
const FUTURE_TOLERANCE_MS: u64 = 1_000;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    job: String,
    request: String,
    created_at_ms: u64,
    control: Control,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileReply {
    job: String,
    request: String,
    record: Record,
    error: Option<String>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

/// The 32-digit lowercase hexadecimal ID after `prefix`, if `name` is exactly
/// that. Rejects separators, so no request can name a path.
fn entry_id<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    let id = name.strip_prefix(prefix)?;
    (id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
    .then_some(id)
}

/// The control directory, only if it is a real private directory owned by
/// the current user.
fn control_dir(job_dir: &Path) -> io::Result<PathBuf> {
    let dir = job_dir.join(CONTROL_DIR);
    let metadata = fs::symlink_metadata(&dir)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(invalid("unsafe control directory"));
    }
    Ok(dir)
}

/// Open without following a symlink or blocking on a FIFO, then accept only a
/// private regular file owned by the current user with a single link.
fn open_private(path: &Path, write: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(invalid("control file is not a private regular file"));
    }
    Ok(file)
}

/// The whole file, rejecting rather than truncating anything over `limit`.
fn read_limited(file: File, limit: u64) -> io::Result<Vec<u8>> {
    if file.metadata()?.len() > limit {
        return Err(invalid("control file exceeds its size limit"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("control file exceeds its size limit"));
    }
    Ok(bytes)
}

/// Write a private file under a temporary name, then rename it into place.
fn publish(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        file.write_all(bytes)?;
        fs::rename(&tmp, dir.join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Remove a directory entry without following it. Unlinking a symlink, FIFO
/// or extra hard link removes only this name.
fn discard(path: &Path) {
    if fs::remove_file(path).is_err() {
        let _ = fs::remove_dir(path);
    }
}

fn abandoned(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .map(|time| {
            SystemTime::now()
                .duration_since(time)
                .is_ok_and(|age| age >= ABANDONED_AFTER)
        })
        .unwrap_or(true)
}

/// Whether a guardian currently holds this job's control lock. `Ok` means a
/// live process holds it; any error means no live endpoint was proven.
pub fn endpoint_live(job_dir: &Path) -> io::Result<()> {
    let file = open_private(&job_dir.join(CONTROL_LOCK), false)?;
    match FileExt::try_lock_shared(&file) {
        // Acquired: nobody holds it exclusively. Dropping the file releases it.
        Ok(()) => Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "no live supervisor holds the control lock",
        )),
        Err(error)
            if error.kind() == io::ErrorKind::WouldBlock
                || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Remove the endpoint. Called by the keeper after the guardian's group is
/// killed and reaped, before its final write. Never follows symlinks.
pub fn remove(job_dir: &Path) {
    let _ = fs::remove_file(job_dir.join(CONTROL_LOCK));
    let dir = job_dir.join(CONTROL_DIR);
    if fs::symlink_metadata(&dir).is_ok_and(|metadata| metadata.is_dir()) {
        let _ = fs::remove_dir_all(&dir);
    } else {
        let _ = fs::remove_file(&dir);
    }
}

/// The guardian's endpoint. Dropping it, or the guardian exiting, releases
/// the lock; the keeper removes the files.
pub struct Endpoint {
    dir: PathBuf,
    job: String,
    _lock: File,
    /// Applied request IDs with the last millisecond their request is valid.
    seen: Vec<(String, u64)>,
    /// The latest window end among forgotten IDs. A request whose window
    /// ends no later is refused, so a backward wall-clock step cannot reopen
    /// the window of a forgotten applied request.
    forgotten_until_ms: u64,
}

/// One claimed request: its ID and either the action or why it is refused.
pub type Claimed = (String, std::result::Result<Control, String>);

impl Endpoint {
    /// Create the endpoint in a fresh job directory. Pre-existing control
    /// files are refused rather than adopted.
    pub fn create(job_dir: &Path, job: &str) -> Result<Self> {
        DirBuilder::new()
            .mode(0o700)
            .create(job_dir.join(CONTROL_DIR))
            .context("cannot create file control directory")?;
        let dir = control_dir(job_dir).context("cannot use file control directory")?;
        let lock = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(job_dir.join(CONTROL_LOCK))
            .context("cannot create control lock")?;
        // Observers hold a shared lock only momentarily.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match FileExt::try_lock_exclusive(&lock) {
                Ok(()) => break,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                Err(error) => return Err(error).context("cannot hold control lock"),
            }
        }
        Ok(Self {
            dir,
            job: job.to_owned(),
            _lock: lock,
            seen: Vec::new(),
            forgotten_until_ms: 0,
        })
    }

    /// Claim a bounded batch of requests and clear bounded leftovers. Claimed
    /// requests are already unlinked; each must be answered with `reply`.
    /// Any request named with an applied ID whose window is still open is
    /// dropped silently, whatever it contains, so it can never be applied
    /// twice or overwrite the original reply.
    pub fn take(&mut self) -> Vec<Claimed> {
        self.take_at(now_ms())
    }

    fn take_at(&mut self, now: u64) -> Vec<Claimed> {
        let mut claimed = Vec::new();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return claimed;
        };
        self.forget_closed(now);
        for entry in entries.take(SCAN_LIMIT) {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                discard(&path);
                continue;
            };
            if let Some(id) = entry_id(name, "req-") {
                if claimed.len() >= BATCH_LIMIT {
                    continue;
                }
                let id = id.to_owned();
                if let Some(result) = self.claim(&path, &id, now) {
                    claimed.push((id, result));
                }
            } else if entry_id(name, "rep-").is_some() || entry_id(name, "tmp-").is_some() {
                if abandoned(&path) {
                    discard(&path);
                }
            } else {
                discard(&path);
            }
        }
        claimed
    }

    /// Forget applied IDs whose request windows have closed. Requests named
    /// by them are stale from now on, so forgetting them cannot admit a
    /// replay.
    fn forget_closed(&mut self, now: u64) {
        let floor = &mut self.forgotten_until_ms;
        self.seen.retain(|(_, valid_until)| {
            let open = *valid_until >= now;
            if !open {
                *floor = (*floor).max(*valid_until);
            }
            open
        });
    }

    /// `None` when the request was withdrawn or duplicated: nothing applies
    /// and nothing is answered.
    fn claim(
        &mut self,
        path: &Path,
        id: &str,
        now: u64,
    ) -> Option<std::result::Result<Control, String>> {
        let opened = open_private(path, false);
        // Claim by unlinking. A client that withdrew first wins instead.
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != io::ErrorKind::NotFound {
                discard(path);
            }
            return None;
        }
        // Checked before reading, so no copy of an applied request, altered
        // or not, is applied or answered while its window is open.
        if self.seen.iter().any(|(seen, _)| seen == id) {
            return None;
        }
        let parsed = opened
            .and_then(|file| read_limited(file, REQUEST_LIMIT))
            .map_err(|error| format!("rejected control request: {error}"))
            .and_then(|bytes| {
                serde_json::from_slice::<Request>(&bytes)
                    .map_err(|_| "rejected malformed control request".to_owned())
            });
        let request = match parsed {
            Ok(request) => request,
            Err(error) => return Some(Err(error)),
        };
        if request.job != self.job || request.request != id {
            return Some(Err("control request does not match this job".into()));
        }
        let budget = crate::lifecycle::CONTROL_EXCHANGE.as_millis() as u64;
        let valid_until = request.created_at_ms.saturating_add(budget);
        if valid_until < now
            || request.created_at_ms > now.saturating_add(FUTURE_TOLERANCE_MS)
            || valid_until <= self.forgotten_until_ms
        {
            return Some(Err("stale control request; not applied".into()));
        }
        // Never evict an ID that is still replayable; refuse new work instead.
        if self.seen.len() >= SEEN_LIMIT {
            return Some(Err(
                "too many recent control requests; not applied, retry later".into(),
            ));
        }
        self.seen.push((id.to_owned(), valid_until));
        Some(Ok(request.control))
    }

    /// Publish the reply for a claimed request. Failures leave the client to
    /// report an unknown outcome; they never retry the action.
    pub fn reply(&self, id: &str, reply: &Reply) {
        let Ok(bytes) = serde_json::to_vec(&FileReply {
            job: self.job.clone(),
            request: id.to_owned(),
            record: reply.record.clone(),
            error: reply.error.clone(),
        }) else {
            return;
        };
        let _ = publish(&self.dir, &format!("rep-{id}"), &bytes);
    }
}

/// One bounded request/reply exchange. A request that was never taken is
/// withdrawn and reported as not applied; one that was taken without a reply
/// is reported as an unknown outcome and is never resent.
pub fn exchange(
    job_dir: &Path,
    job: &str,
    action: &Control,
    deadline: Instant,
    reply_limit: u64,
) -> Result<Reply> {
    endpoint_live(job_dir).map_err(|error| {
        anyhow::anyhow!("supervisor unavailable ({error}); refusing to signal a PID from disk")
    })?;
    let dir = control_dir(job_dir).context("cannot use file control directory")?;
    let pending = fs::read_dir(&dir)?.take(QUEUE_LIMIT).count();
    if pending >= QUEUE_LIMIT {
        bail!("control queue is full; nothing was sent");
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let bytes = serde_json::to_vec(&Request {
        job: job.to_owned(),
        request: id.clone(),
        created_at_ms: now_ms(),
        control: action.clone(),
    })?;
    if bytes.len() as u64 > REQUEST_LIMIT {
        bail!("control request exceeds {REQUEST_LIMIT} bytes");
    }
    let request_path = dir.join(format!("req-{id}"));
    let reply_path = dir.join(format!("rep-{id}"));
    publish(&dir, &format!("req-{id}"), &bytes).context("cannot publish control request")?;
    loop {
        match open_private(&reply_path, false) {
            Ok(file) => {
                let bytes = read_limited(file, reply_limit);
                let _ = fs::remove_file(&reply_path);
                let reply: FileReply = serde_json::from_slice(&bytes?)
                    .context("malformed control reply; the outcome is unknown")?;
                if reply.job != job || reply.request != id || reply.record.id != job {
                    bail!("control reply does not match this request; the outcome is unknown");
                }
                return Ok(Reply {
                    record: reply.record,
                    error: reply.error,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => {
                discard(&reply_path);
                bail!("unsafe control reply ({error}); the outcome is unknown");
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    match fs::remove_file(&request_path) {
        Ok(()) => bail!(
            "supervisor did not take the control request within its bound; \
             it was withdrawn and not applied"
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => bail!(
            "control request was taken but no reply arrived within its bound; \
             the outcome is unknown and the request was not resent"
        ),
        Err(error) => {
            bail!("control request could not be withdrawn ({error}); the outcome is unknown")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Launch, State};

    fn job_dir() -> tempfile::TempDir {
        let mut builder = tempfile::Builder::new();
        builder.prefix("c");
        let dir = match std::env::var_os("PROCLEASE_TEST_ROOT") {
            Some(root) => {
                fs::create_dir_all(&root).unwrap();
                builder.tempdir_in(root).unwrap()
            }
            None => builder.tempdir().unwrap(),
        };
        let mut permissions = fs::metadata(dir.path()).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
        fs::set_permissions(dir.path(), permissions).unwrap();
        dir
    }

    const JOB: &str = "00000000000f";

    fn record() -> Record {
        let now = now_ms();
        Record::new(&Launch {
            id: JOB.into(),
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
            grace_ms: 0,
            log_limit_bytes: 1,
            mode: crate::model::Mode::Foreground,
            transport: crate::model::Transport::File,
        })
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    fn request(id: &str, created_at_ms: u64) -> Vec<u8> {
        serde_json::to_vec(&Request {
            job: JOB.into(),
            request: id.into(),
            created_at_ms,
            control: Control::Extend { by_ms: 1 },
        })
        .unwrap()
    }

    fn hex(byte: char) -> String {
        std::iter::repeat_n(byte, 32).collect()
    }

    #[test]
    fn lock_proves_liveness_only_while_the_endpoint_lives() {
        let dir = job_dir();
        assert!(endpoint_live(dir.path()).is_err(), "no endpoint yet");
        let endpoint = Endpoint::create(dir.path(), JOB).unwrap();
        endpoint_live(dir.path()).unwrap();
        // A second endpoint can never adopt existing control files.
        assert!(Endpoint::create(dir.path(), JOB).is_err());
        drop(endpoint);
        let error = endpoint_live(dir.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotConnected);
        // A replaced, symlinked or FIFO lock never proves liveness.
        remove(dir.path());
        std::os::unix::fs::symlink("/dev/null", dir.path().join(CONTROL_LOCK)).unwrap();
        assert!(endpoint_live(dir.path()).is_err());
        fs::remove_file(dir.path().join(CONTROL_LOCK)).unwrap();
        let path =
            std::ffi::CString::new(dir.path().join(CONTROL_LOCK).as_os_str().as_encoded_bytes())
                .unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(endpoint_live(dir.path()).is_err());
    }

    #[test]
    fn requests_apply_once_and_hostile_entries_are_bounded() {
        let dir = job_dir();
        let mut endpoint = Endpoint::create(dir.path(), JOB).unwrap();
        let ctl = dir.path().join(CONTROL_DIR);
        let now = now_ms();
        let valid = hex('a');
        write_private(&ctl.join(format!("req-{valid}")), &request(&valid, now));
        // Stale, future, foreign-job, mismatched-ID, oversize and malformed.
        let stale = hex('b');
        write_private(
            &ctl.join(format!("req-{stale}")),
            &request(&stale, now - 5_000),
        );
        let future = hex('c');
        write_private(
            &ctl.join(format!("req-{future}")),
            &request(&future, now + 60_000),
        );
        let mismatched = hex('d');
        write_private(
            &ctl.join(format!("req-{mismatched}")),
            &request(&valid, now),
        );
        let oversize = hex('e');
        write_private(
            &ctl.join(format!("req-{oversize}")),
            &vec![b' '; REQUEST_LIMIT as usize + 1],
        );
        let malformed = hex('f');
        write_private(
            &ctl.join(format!("req-{malformed}")),
            b"{\"control\":\"run\"}",
        );
        // A symlink, FIFO, hard link and traversal-like junk are not followed.
        let outside = dir.path().join("outside");
        write_private(&outside, &request(&hex('1'), now));
        let symlink = hex('1');
        std::os::unix::fs::symlink(&outside, ctl.join(format!("req-{symlink}"))).unwrap();
        let hardlink = hex('2');
        fs::hard_link(&outside, ctl.join(format!("req-{hardlink}"))).unwrap();
        let fifo = hex('3');
        let path = std::ffi::CString::new(
            ctl.join(format!("req-{fifo}"))
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        // Names outside the fixed grammar are removed, never interpreted.
        write_private(&ctl.join("req-ABC"), &request(&hex('4'), now));
        fs::create_dir(ctl.join("junk")).unwrap();

        let started = Instant::now();
        let mut claimed = endpoint.take();
        claimed.extend(endpoint.take());
        assert!(started.elapsed() < Duration::from_secs(1), "FIFO blocked");
        let applied: Vec<_> = claimed
            .iter()
            .filter(|(_, result)| result.is_ok())
            .map(|(id, _)| id.clone())
            .collect();
        assert_eq!(applied, vec![valid.clone()]);
        let refused = claimed.iter().filter(|(_, r)| r.is_err()).count();
        assert_eq!(refused, 8, "every hostile request is answered, not applied");
        // Every hostile entry was removed; the target survived untouched.
        assert_eq!(fs::read_dir(&ctl).unwrap().count(), 0);
        assert_eq!(fs::read(&outside).unwrap(), request(&hex('1'), now));

        // Replaying an applied request is dropped without a reply.
        write_private(
            &ctl.join(format!("req-{valid}")),
            &request(&valid, now_ms()),
        );
        assert!(endpoint.take().is_empty());
        assert_eq!(fs::read_dir(&ctl).unwrap().count(), 0);
    }

    #[test]
    fn exchange_withdraws_untaken_requests_and_reports_taken_ones_as_unknown() {
        let dir = job_dir();
        let mut endpoint = Endpoint::create(dir.path(), JOB).unwrap();
        let ctl = dir.path().join(CONTROL_DIR);
        // Never served: the request is withdrawn and nothing remains.
        let error = exchange(
            dir.path(),
            JOB,
            &Control::Stop,
            Instant::now() + Duration::from_millis(50),
            1 << 20,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("withdrawn and not applied"), "{error}");
        assert_eq!(fs::read_dir(&ctl).unwrap().count(), 0);
        assert!(endpoint.take().is_empty(), "withdrawn request was applied");

        // Served: the reply is correlated and consumed.
        let served = thread::scope(|scope| {
            let server = scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    if let Some((id, result)) = endpoint.take().into_iter().next() {
                        assert!(matches!(result, Ok(Control::Extend { by_ms: 7 })));
                        let mut record = record();
                        record.state = State::Running;
                        endpoint.reply(
                            &id,
                            &Reply {
                                record,
                                error: None,
                            },
                        );
                        return true;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                false
            });
            let reply = exchange(
                dir.path(),
                JOB,
                &Control::Extend { by_ms: 7 },
                Instant::now() + Duration::from_secs(2),
                1 << 20,
            );
            assert!(server.join().unwrap());
            reply.unwrap()
        });
        assert_eq!(served.record.id, JOB);
        assert_eq!(fs::read_dir(&ctl).unwrap().count(), 0);

        // Taken without a reply: unknown outcome, never replayed.
        let error = thread::scope(|scope| {
            let server = scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    if !endpoint.take().is_empty() {
                        return;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            });
            let error = exchange(
                dir.path(),
                JOB,
                &Control::Extend { by_ms: 1 },
                Instant::now() + Duration::from_millis(300),
                1 << 20,
            )
            .unwrap_err()
            .to_string();
            server.join().unwrap();
            error
        });
        assert!(error.contains("outcome is unknown"), "{error}");

        // One entry below the limit still admits a request (withdrawn here
        // because nobody serves it); exactly at the limit nothing is sent.
        for index in 0..QUEUE_LIMIT - 1 {
            write_private(&ctl.join(format!("tmp-{index:032x}")), b"");
        }
        let error = exchange(
            dir.path(),
            JOB,
            &Control::Stop,
            Instant::now() + Duration::from_millis(50),
            1 << 20,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("withdrawn and not applied"), "{error}");
        assert_eq!(fs::read_dir(&ctl).unwrap().count(), QUEUE_LIMIT - 1);
        write_private(&ctl.join(format!("tmp-{:032x}", QUEUE_LIMIT)), b"");
        assert_eq!(fs::read_dir(&ctl).unwrap().count(), QUEUE_LIMIT);
        let error = exchange(
            dir.path(),
            JOB,
            &Control::Stop,
            Instant::now() + Duration::from_millis(50),
            1 << 20,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("queue is full"), "{error}");
        assert_eq!(fs::read_dir(&ctl).unwrap().count(), QUEUE_LIMIT);
        drop(endpoint);
        let error = exchange(
            dir.path(),
            JOB,
            &Control::Stop,
            Instant::now() + Duration::from_millis(50),
            1 << 20,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("supervisor unavailable"), "{error}");
    }

    #[test]
    fn abandoned_replies_and_partial_writes_are_collected() {
        let dir = job_dir();
        let mut endpoint = Endpoint::create(dir.path(), JOB).unwrap();
        let ctl = dir.path().join(CONTROL_DIR);
        let old = SystemTime::now() - ABANDONED_AFTER - Duration::from_secs(1);
        for name in [format!("rep-{}", hex('a')), format!("tmp-{}", hex('b'))] {
            write_private(&ctl.join(&name), b"{}");
            File::options()
                .write(true)
                .open(ctl.join(&name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let fresh = format!("rep-{}", hex('c'));
        write_private(&ctl.join(&fresh), b"{}");
        assert!(endpoint.take().is_empty());
        let left: Vec<_> = fs::read_dir(&ctl)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, vec![fresh]);
    }

    const BUDGET: u64 = crate::lifecycle::CONTROL_EXCHANGE.as_millis() as u64;

    fn publish_request(dir: &Path, id: &str, created_at_ms: u64) {
        write_private(
            &dir.join(CONTROL_DIR).join(format!("req-{id}")),
            &request(id, created_at_ms),
        );
    }

    fn only_claim(claimed: Vec<Claimed>, id: &str) -> std::result::Result<Control, String> {
        match <[Claimed; 1]>::try_from(claimed) {
            Ok([(claimed_id, result)]) if claimed_id == id => result,
            Ok([(other, _)]) => panic!("claimed {other}, expected {id}"),
            Err(claimed) => panic!("expected one claim of {id}, got {}", claimed.len()),
        }
    }

    #[test]
    fn cache_pressure_refuses_new_requests_instead_of_forgetting_replayable_ids() {
        let dir = job_dir();
        let mut endpoint = Endpoint::create(dir.path(), JOB).unwrap();
        let ctl = dir.path().join(CONTROL_DIR);
        let created = now_ms();
        for index in 0..SEEN_LIMIT {
            let id = format!("{index:032x}");
            publish_request(dir.path(), &id, created);
            assert!(only_claim(endpoint.take_at(created), &id).is_ok());
        }
        // Memory is full of open windows: one more distinct valid request is
        // answered as refused and not applied, and nothing is evicted.
        let extra = format!("{SEEN_LIMIT:032x}");
        publish_request(dir.path(), &extra, created);
        let error = only_claim(endpoint.take_at(created), &extra).unwrap_err();
        assert!(
            error.contains("too many recent control requests"),
            "{error}"
        );
        // An identical copy of the oldest applied request, at the last
        // millisecond of its original window, is dropped without an answer.
        let first = format!("{:032x}", 0);
        for now in [created, created + BUDGET] {
            publish_request(dir.path(), &first, created);
            assert!(endpoint.take_at(now).is_empty(), "replay applied at {now}");
            assert_eq!(fs::read_dir(&ctl).unwrap().count(), 0);
        }
        // Once the window has closed the copy is stale, and memory is free
        // for new requests again.
        let after = created + BUDGET + 1;
        publish_request(dir.path(), &first, created);
        let error = only_claim(endpoint.take_at(after), &first).unwrap_err();
        assert!(error.contains("stale"), "{error}");
        assert!(endpoint.seen.is_empty());
        let fresh = hex('f');
        publish_request(dir.path(), &fresh, after);
        assert!(only_claim(endpoint.take_at(after), &fresh).is_ok());
    }

    #[test]
    fn future_stamped_requests_are_remembered_for_their_whole_window() {
        let dir = job_dir();
        let mut endpoint = Endpoint::create(dir.path(), JOB).unwrap();
        let ctl = dir.path().join(CONTROL_DIR);
        let now = now_ms();
        let edge = now + FUTURE_TOLERANCE_MS;
        // The latest accepted creation time, and one past it.
        let id = hex('a');
        publish_request(dir.path(), &id, edge);
        assert!(only_claim(endpoint.take_at(now), &id).is_ok());
        let ahead = hex('b');
        publish_request(dir.path(), &ahead, edge + 1);
        assert!(only_claim(endpoint.take_at(now), &ahead).is_err());

        // Answer the applied request, then replay it, identical or altered.
        endpoint.reply(
            &id,
            &Reply {
                record: record(),
                error: None,
            },
        );
        let reply_path = ctl.join(format!("rep-{id}"));
        let original = fs::read(&reply_path).unwrap();
        let altered = serde_json::to_vec(&Request {
            job: "other".into(),
            request: id.clone(),
            created_at_ms: edge,
            control: Control::Stop,
        })
        .unwrap();
        // Past the apply time plus the budget, but still inside the window
        // that the request's own future timestamp opened.
        for at in [now + BUDGET + 1, edge + BUDGET] {
            publish_request(dir.path(), &id, edge);
            assert!(endpoint.take_at(at).is_empty(), "replay applied at {at}");
            write_private(&ctl.join(format!("req-{id}")), &altered);
            assert!(endpoint.take_at(at).is_empty(), "altered copy answered");
            assert_eq!(fs::read(&reply_path).unwrap(), original);
        }
        fs::remove_file(&reply_path).unwrap();

        // After the window the copy is stale and the ID is forgotten.
        let closed = edge + BUDGET + 1;
        publish_request(dir.path(), &id, edge);
        assert!(only_claim(endpoint.take_at(closed), &id).is_err());
        assert!(endpoint.seen.is_empty());
        // A backward wall-clock step cannot reopen that forgotten window:
        // requests whose windows end no later are refused, not applied.
        for (replay, created) in [(id.clone(), edge), (hex('c'), now)] {
            publish_request(dir.path(), &replay, created);
            let error = only_claim(endpoint.take_at(now), &replay).unwrap_err();
            assert!(error.contains("stale"), "{error}");
        }
        // A request whose window ends later than any forgotten one applies.
        let later = hex('d');
        publish_request(dir.path(), &later, edge + 1);
        assert!(only_claim(endpoint.take_at(edge + 1), &later).is_ok());
        assert_eq!(fs::read_dir(&ctl).unwrap().count(), 0);
    }
}
