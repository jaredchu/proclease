#![cfg(unix)]

use serde_json::Value;
use std::{
    fs,
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::{Command, Output},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let mut builder = tempfile::Builder::new();
        builder.prefix("p");
        let dir = if let Some(root) = std::env::var_os("PROCLEASE_TEST_ROOT") {
            fs::create_dir_all(&root).unwrap();
            builder.tempdir_in(root).unwrap()
        } else {
            builder.tempdir().unwrap()
        };
        let state = dir.path().join("s");
        Self { dir, state }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_proclease"));
        cmd.arg("--state-dir")
            .arg(&self.state)
            .arg("--json")
            .args(args);
        cmd
    }
    fn call(&self, args: &[&str]) -> Value {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        parse(&output)
    }
    fn start(&self, mode: &str, ttl: &str) -> (String, PathBuf) {
        let ready = self
            .dir
            .path()
            .join(format!("r-{}.json", uuid::Uuid::new_v4().simple()));
        let service = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/service.py");
        let result = self.call(&[
            "run",
            "--ttl",
            ttl,
            "--grace",
            "100ms",
            "--owner",
            "test",
            "--",
            "/usr/bin/python3",
            service.to_str().unwrap(),
            mode,
            ready.to_str().unwrap(),
        ]);
        let id = result["job"]["id"].as_str().unwrap().to_owned();
        (id, ready)
    }
    fn job(&self, id: &str) -> Value {
        self.call(&["list", "--all"])["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["id"] == id)
            .unwrap()
            .clone()
    }
    #[track_caller]
    fn terminal(&self, id: &str) -> Value {
        self.terminal_within(id, Duration::from_secs(8))
    }
    #[track_caller]
    fn terminal_within(&self, id: &str, timeout: Duration) -> Value {
        wait_for(timeout, || {
            let job = self.job(id);
            match job["state"].as_str().unwrap() {
                "stopped" | "expired" | "exited" | "failed" | "interrupted" => Some(job),
                _ => None,
            }
        })
    }
    fn finished_template(&self) -> (Value, Value) {
        self.template(&["run", "--grace", "100ms", "--", "/usr/bin/true"])
    }
    /// Run a short command, then remove its job and return its records.
    /// The launch record carries this test process's context fingerprint.
    fn template(&self, args: &[&str]) -> (Value, Value) {
        let result = self.call(args);
        let id = result["job"]["id"].as_str().unwrap();
        let record = self.terminal(id);
        let pid = record["keeper_pid"].as_u64().unwrap() as i32;
        wait(|| {
            (unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH))
            .then_some(())
        });
        let dir = self.state.join(id);
        let record = serde_json::from_slice(&fs::read(dir.join("record.json")).unwrap()).unwrap();
        let launch = serde_json::from_slice(&fs::read(dir.join("launch.json")).unwrap()).unwrap();
        fs::remove_dir_all(dir).unwrap();
        (record, launch)
    }
    fn seed_history(&self, id: &str, record: &Value, launch: &Value, finished: u64) {
        let mut record = record.clone();
        record["id"] = serde_json::json!(id);
        record["finished_at_ms"] = serde_json::json!(finished);
        let mut launch = launch.clone();
        launch["id"] = serde_json::json!(id);
        self.seed_files(
            id,
            &[
                ("record.json", serde_json::to_vec(&record).unwrap()),
                ("launch.json", serde_json::to_vec(&launch).unwrap()),
                ("output.log", b"retained test output".to_vec()),
            ],
        );
    }
    /// Commit a launch exactly as `run` does before starting its keeper, with
    /// the persisted lease shifted to begin at `created`.
    fn seed_starting(&self, id: &str, record: &Value, launch: &Value, created: u64) {
        let ttl =
            launch["expires_at_ms"].as_u64().unwrap() - launch["created_at_ms"].as_u64().unwrap();
        let mut launch = launch.clone();
        launch["id"] = serde_json::json!(id);
        launch["created_at_ms"] = serde_json::json!(created);
        launch["expires_at_ms"] = serde_json::json!(created + ttl);
        let mut record = record.clone();
        for (field, value) in [
            ("id", serde_json::json!(id)),
            ("created_at_ms", launch["created_at_ms"].clone()),
            ("expires_at_ms", launch["expires_at_ms"].clone()),
            ("state", serde_json::json!("starting")),
            ("keeper_pid", serde_json::json!(0)),
            ("logs_truncated", serde_json::json!(false)),
        ] {
            record[field] = value;
        }
        for field in [
            "reason",
            "guardian_pid",
            "command_pid",
            "exit_code",
            "finished_at_ms",
            "error",
        ] {
            record[field] = Value::Null;
        }
        self.seed_files(
            id,
            &[
                ("record.json", serde_json::to_vec(&record).unwrap()),
                ("launch.json", serde_json::to_vec(&launch).unwrap()),
            ],
        );
    }
    fn seed_files(&self, id: &str, files: &[(&str, Vec<u8>)]) {
        use std::os::unix::fs::PermissionsExt;
        let dir = self.state.join(id);
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        for (name, bytes) in files {
            fs::write(dir.join(name), bytes).unwrap();
            fs::set_permissions(dir.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    /// Run the hidden keeper in the foreground for a seeded starting job. It
    /// returns only after its guardian group is cleaned up and state is final.
    fn keep(&self, id: &str) -> (Output, Value) {
        let output = self.command(&["__keeper", id]).output().unwrap();
        let record =
            serde_json::from_slice(&fs::read(self.state.join(id).join("record.json")).unwrap())
                .unwrap();
        (output, record)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.state.exists() {
            return;
        }
        if thread::panicking() {
            if let Ok(entries) = fs::read_dir(&self.state) {
                for entry in entries.flatten() {
                    for name in ["record.json", "supervisor.log", "output.log"] {
                        if let Ok(content) = fs::read(entry.path().join(name)) {
                            eprintln!(
                                "fixture {} / {name}: {}",
                                entry.file_name().to_string_lossy(),
                                String::from_utf8_lossy(&content[..content.len().min(8192)])
                            );
                        }
                    }
                }
            }
        }
        let output = self.command(&["list"]).output().unwrap();
        if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
            if let Some(jobs) = value["jobs"].as_array() {
                for job in jobs {
                    if let Some(id) = job["id"].as_str() {
                        let _ = self.command(&["stop", id]).output();
                    }
                }
            }
        }
        // A job can be observed as finished, and `stop` can return, before its
        // keeper has made its final record write, removed its socket and
        // collected history under the root lock. Deleting the tree in that
        // window races those writes and leaves partial job directories. Wait
        // for every supervisor of this state; never delete beneath a live one.
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let live = self.supervisors();
            if live.is_empty() {
                return;
            }
            if Instant::now() >= deadline {
                self.dir.disable_cleanup(true);
                let message = format!(
                    "supervisors {live:?} still own fixture state; retained {}",
                    self.dir.path().display()
                );
                if thread::panicking() {
                    eprintln!("{message}");
                } else {
                    panic!("{message}");
                }
                return;
            }
            thread::sleep(Duration::from_millis(35));
        }
    }
}

impl Fixture {
    /// Live keeper/guardian processes whose argument vector names this
    /// fixture's canonical state root. Identification uses the running
    /// process, not PIDs copied from (possibly seeded) records, and never
    /// sends a signal.
    fn supervisors(&self) -> Vec<u32> {
        match fs::canonicalize(&self.state) {
            Ok(root) => supervisors_of(&root),
            Err(_) => Vec::new(),
        }
    }
}

/// Running (non-zombie) keeper/guardian processes whose argument vector names
/// `root`, a canonical state root. Matching is by the live process's argv, not
/// by PID alone, so a recycled PID owned by an unrelated process never matches,
/// and an exited but unreaped (zombie) supervisor, which can no longer write,
/// is not reported. Works after `root` has been deleted.
fn supervisors_of(root: &std::path::Path) -> Vec<u32> {
    use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let mut live: Vec<u32> = system
        .processes()
        .iter()
        .filter(|(_, process)| {
            let cmd = process.cmd();
            process.status() != ProcessStatus::Zombie
                && process.thread_kind().is_none()
                && cmd.len() == 5
                && cmd[1] == "--state-dir"
                && root == std::path::Path::new(&cmd[2])
                && (cmd[3] == "__keeper" || cmd[3] == "__guard")
        })
        .map(|(pid, _)| pid.as_u32())
        .collect();
    live.sort_unstable();
    live
}

fn parse(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("invalid JSON: {}", String::from_utf8_lossy(&output.stdout)))
}

#[track_caller]
fn wait<T>(predicate: impl FnMut() -> Option<T>) -> T {
    wait_for(Duration::from_secs(8), predicate)
}

#[track_caller]
fn wait_for<T>(timeout: Duration, mut predicate: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = predicate() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for lifecycle condition"
        );
        thread::sleep(Duration::from_millis(35));
    }
}

#[track_caller]
fn ready(path: &std::path::Path) -> Value {
    wait(|| {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    })
}

#[track_caller]
fn assert_port_closed(info: &Value) {
    let address: SocketAddr = format!("127.0.0.1:{}", info["port"].as_u64().unwrap())
        .parse()
        .unwrap();
    wait(|| {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err() {
            Some(())
        } else {
            None
        }
    });
}

#[test]
fn expiry_kills_term_resistant_children_after_launcher_returns() {
    let fixture = Fixture::new();
    let started = Instant::now();
    // The readiness helper allows eight seconds for cold interpreter startup.
    // Keep that inside the lease; expiry must not race the readiness assertion.
    let (id, path) = fixture.start("stubborn", "10s");
    eprintln!(
        "expiry fixture: launch returned after {:?}",
        started.elapsed()
    );
    let info = ready(&path);
    eprintln!("expiry fixture: ready after {:?}", started.elapsed());
    let expiry = fixture.job(&id)["expires_at_ms"].as_u64().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    // Bound cleanup relative to the actual expiry, not the time startup took.
    let remaining = Duration::from_millis(expiry.saturating_sub(now));
    let job = fixture.terminal_within(&id, remaining + Duration::from_secs(2));
    eprintln!("expiry fixture: terminal after {:?}", started.elapsed());
    assert_eq!(job["state"], "expired");
    assert_eq!(job["reason"], "deadline");
    assert_port_closed(&info);
}

#[test]
fn expiry_does_not_wait_for_application_readiness() {
    let fixture = Fixture::new();
    let marker = fixture.dir.path().join("should-not-be-ready");
    let result = fixture.call(&[
        "run",
        "--ttl",
        "500ms",
        "--grace",
        "100ms",
        "--",
        "/bin/sh",
        "-c",
        "sleep 30; printf ready > \"$1\"",
        "startup-fixture",
        marker.to_str().unwrap(),
    ]);
    let id = result["job"]["id"].as_str().unwrap();
    let job = fixture.terminal(id);
    assert_eq!(job["state"], "expired");
    assert_eq!(job["reason"], "deadline");
    // Under heavy load the short lease can pass before the guardian spawns
    // the command; it then expires without ever starting it.
    let Some(pid) = result["job"]["command_pid"].as_u64() else {
        assert!(job["command_pid"].is_null());
        assert!(!marker.exists());
        return;
    };
    let pid = pid as i32;
    wait(|| {
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            Some(())
        } else {
            None
        }
    });
    assert!(!marker.exists(), "expiry must prevent later readiness");
}

#[test]
fn default_views_show_recent_completion_then_history_but_keep_unverified_jobs() {
    let fixture = Fixture::new();
    let args = [
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--key",
        "history-job",
        "--",
        "/bin/sleep",
        "30",
    ];
    let launched = fixture.call(&args);
    let id = launched["job"]["id"].as_str().unwrap();
    assert_eq!(fixture.call(&["list"])["jobs"][0]["id"], id);
    assert_eq!(fixture.call(&["stop", id])["job"]["state"], "stopped");

    for view in [&["list"][..], &["watch", "--once"][..]] {
        assert_eq!(fixture.call(view)["jobs"][0]["state"], "stopped");
    }
    for view in [&["list", "--all"][..], &["watch", "--once", "--all"][..]] {
        let jobs = fixture.call(view);
        assert_eq!(jobs["jobs"][0]["id"], id);
        assert_eq!(jobs["jobs"][0]["state"], "stopped");
    }
    let logs = fixture.call(&["logs", id]);
    assert_eq!(logs["id"], id);
    let retry = fixture.call(&args);
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["job"]["id"], id);
    assert_eq!(retry["job"]["state"], "stopped");

    let record_path = fixture.state.join(id).join("record.json");
    let mut record = wait(|| {
        let record: Value = serde_json::from_slice(&fs::read(&record_path).ok()?).ok()?;
        (record["state"] == "stopped").then_some(record)
    });
    record["finished_at_ms"] = serde_json::json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - 31_000
    );
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    for view in [&["list"][..], &["watch", "--once"][..]] {
        assert_eq!(fixture.call(view)["jobs"], serde_json::json!([]));
    }
    assert_eq!(fixture.call(&["list", "--all"])["jobs"][0]["id"], id);
    // Unavailable supervision is not evidence of completion. Keep this state
    // visible even though routine finished history is hidden.
    record["state"] = serde_json::json!("running");
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    for view in [&["list"][..], &["watch", "--once"][..]] {
        assert_eq!(fixture.call(view)["jobs"][0]["state"], "unknown");
    }
}

#[test]
fn recent_finished_views_are_capped_without_hiding_active_or_unverified_jobs() {
    let fixture = Fixture::new();
    let (record, launch) = fixture.finished_template();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    for i in 1..=8 {
        fixture.seed_history(&format!("{i:012x}"), &record, &launch, now - i);
    }
    let mut unknown = record.clone();
    unknown["state"] = serde_json::json!("unknown");
    fixture.seed_history("fffffffffff1", &unknown, &launch, 1);
    for view in [&["list"][..], &["watch", "--once"][..]] {
        let shown = fixture.call(view);
        let jobs = shown["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 6);
        assert!(jobs.iter().any(|j| j["state"] == "unknown"));
        for i in 1..=5 {
            assert!(jobs.iter().any(|j| j["id"] == format!("{i:012x}")));
        }
    }
    let measured = fixture.call(&["list", "--all", "--resources"]);
    assert!(measured["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|job| job["resources"].is_null()));
    assert_eq!(
        fixture.call(&["list", "--all"])["jobs"]
            .as_array()
            .unwrap()
            .len(),
        9
    );
}

#[test]
fn resource_metrics_include_busy_children_and_do_not_renew_or_survive_completion() {
    let fixture = Fixture::new();
    let (id, path) = fixture.start("resource-child", "30s");
    ready(&path);
    let expiry = fixture.job(&id)["expires_at_ms"].clone();
    assert!(fixture.call(&["list"])["jobs"][0]
        .get("resources")
        .is_none());
    let usage = wait(|| {
        let view = fixture.call(&["list", "--resources", "--all"]);
        let usage = &view["jobs"][0]["resources"];
        eprintln!("resource sample: {usage}");
        (usage["processes"] == 2
            && usage["memory_rss_bytes"]
                .as_u64()
                .is_some_and(|m| m >= 32 * 1024 * 1024)
            && usage["cpu_percent"].as_f64().is_some_and(|cpu| cpu > 0.1))
        .then_some(usage.clone())
    });
    assert!(usage["sample_interval_ms"].as_u64().unwrap() >= 100);
    assert_eq!(fixture.job(&id)["expires_at_ms"], expiry);
    let view = fixture.call(&["watch", "--once", "--resources", "--all"]);
    assert_eq!(view["jobs"][0]["resources"]["processes"], 2);
    fixture.call(&["stop", &id]);
    assert!(fixture.call(&["list", "--all", "--resources"])["jobs"][0]["resources"].is_null());
}

#[test]
fn human_tables_respect_width_and_color_without_affecting_json() {
    use unicode_width::UnicodeWidthStr;
    let fixture = Fixture::new();
    let launched = fixture.call(&[
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--task",
        "frontend-preview",
        "--",
        "/bin/sleep",
        "30",
    ]);
    let id = launched["job"]["id"].as_str().unwrap();
    for width in [40, 80, 120] {
        let output = Command::new(env!("CARGO_BIN_EXE_proclease"))
            .arg("--state-dir")
            .arg(&fixture.state)
            .args(["list", "--color", "never"])
            .env("COLUMNS", width.to_string())
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(id) && text.contains("running"));
        assert!(!text.contains('\x1b'));
        assert!(text.lines().all(|line| line.width() <= width));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_proclease"))
        .arg("--state-dir")
        .arg(&fixture.state)
        .args(["watch", "--once", "--color", "always"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout).unwrap().contains('\x1b'));
    assert_eq!(
        fixture.call(&["list", "--color", "always"])["jobs"][0]["id"],
        id
    );
}

#[test]
fn retention_dry_run_preserves_data_and_expired_keys_can_be_reused() {
    let fixture = Fixture::new();
    let args = [
        "run",
        "--grace",
        "100ms",
        "--key",
        "retained-key",
        "--",
        "/usr/bin/true",
    ];
    let result = fixture.call(&args);
    let id = result["job"]["id"].as_str().unwrap();
    let done = fixture.terminal(id);
    let keeper = done["keeper_pid"].as_u64().unwrap() as i32;
    wait(|| (unsafe { libc::kill(keeper, 0) } == -1).then_some(()));
    let dir = fixture.state.join(id);
    let mut record: Value =
        serde_json::from_slice(&fs::read(dir.join("record.json")).unwrap()).unwrap();
    record["finished_at_ms"] = serde_json::json!(1);
    fs::write(
        dir.join("record.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let preview = fixture.call(&["prune", "--dry-run"]);
    assert_eq!(preview["candidates"], serde_json::json!([id]));
    assert_eq!(preview["removed"], serde_json::json!([]));
    assert!(dir.join("output.log").exists());
    let next = fixture.call(&args);
    assert_ne!(next["job"]["id"], id);
    assert_eq!(next["reused"], false);
    assert!(!dir.exists(), "age cleanup must remove metadata and logs");
}

#[test]
fn retention_count_is_serialized_and_preserves_active_unverified_and_live_owners() {
    let fixture = Fixture::new();
    let (record, launch) = fixture.finished_template();
    let active = fixture.call(&[
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--",
        "/bin/sleep",
        "30",
    ]);
    let active_id = active["job"]["id"].as_str().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    for i in 1..=104 {
        fixture.seed_history(&format!("{i:012x}"), &record, &launch, now - i);
    }
    let mut unknown = record.clone();
    unknown["state"] = serde_json::json!("unknown");
    fixture.seed_history("fffffffffff1", &unknown, &launch, 1);
    let mut owned = record.clone();
    owned["keeper_pid"] = serde_json::json!(std::process::id());
    fixture.seed_history("fffffffffff2", &owned, &launch, 1);
    let mut live_group = record.clone();
    live_group["guardian_pid"] = serde_json::json!(unsafe { libc::getpgrp() });
    fixture.seed_history("fffffffffff3", &live_group, &launch, 1);
    let mut missing_finish = record.clone();
    missing_finish["id"] = serde_json::json!("fffffffffff4");
    fixture.seed_history("fffffffffff4", &missing_finish, &launch, 1);
    let path = fixture.state.join("fffffffffff4/record.json");
    missing_finish["finished_at_ms"] = Value::Null;
    fs::write(path, serde_json::to_vec(&missing_finish).unwrap()).unwrap();
    let preview = fixture.call(&["prune", "--dry-run"]);
    assert_eq!(preview["candidates"].as_array().unwrap().len(), 4);
    assert_eq!(preview["removed"], serde_json::json!([]));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let mut cmd = fixture.command(&["prune"]);
            thread::spawn(move || {
                let output = cmd.output().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stdout)
                );
                parse(&output)["removed"].as_array().unwrap().clone()
            })
        })
        .collect();
    let removed: Vec<_> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    assert_eq!(removed.len(), 4);
    for i in 1..=104 {
        assert_eq!(fixture.state.join(format!("{i:012x}")).exists(), i <= 100);
    }
    for id in [
        active_id,
        "fffffffffff1",
        "fffffffffff2",
        "fffffffffff3",
        "fffffffffff4",
    ] {
        assert!(
            fixture.state.join(id).exists(),
            "protected job {id} was collected"
        );
    }
    assert_eq!(fixture.job(active_id)["state"], "running");
}

#[test]
fn retention_retries_partial_deletion_without_following_symlinks() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let (record, launch) = fixture.finished_template();
    let outside = fixture.dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"outside job state").unwrap();
    fixture.seed_history("000000000001", &record, &launch, 1);
    std::os::unix::fs::symlink(&outside, fixture.state.join("000000000001/link")).unwrap();
    let pending = fixture
        .state
        .join(".prune-000000000002-0123456789abcdef0123456789abcdef");
    fs::create_dir(&pending).unwrap();
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(pending.join("partial"), b"interrupted cleanup").unwrap();
    let unsafe_pending = fixture
        .state
        .join(".prune-000000000003-0123456789abcdef0123456789abcdef");
    std::os::unix::fs::symlink(&outside, &unsafe_pending).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.state.join("000000000004")).unwrap();
    let output = fixture.command(&["prune"]).output().unwrap();
    assert!(!output.status.success(), "unsafe state must be reported");
    let report = parse(&output);
    assert_eq!(report["removed"].as_array().unwrap().len(), 2);
    assert!(!fixture.state.join("000000000001").exists());
    assert!(!pending.exists());
    assert!(unsafe_pending.is_symlink());
    assert!(fixture.state.join("000000000004").is_symlink());
    assert_eq!(
        fs::read(outside.join("keep")).unwrap(),
        b"outside job state"
    );
}

#[test]
fn job_completion_collects_old_history_without_another_cli_call() {
    let fixture = Fixture::new();
    let (record, launch) = fixture.finished_template();
    fixture.call(&["run", "--grace", "100ms", "--", "/bin/sleep", "1"]);
    fixture.seed_history("000000000001", &record, &launch, 1);
    wait(|| (!fixture.state.join("000000000001").exists()).then_some(()));
}

#[test]
fn failed_history_deletion_is_quarantined_and_does_not_block_keyed_launches() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let fixture = Fixture::new();
    let (record, launch) = fixture.finished_template();
    fixture.seed_history("000000000001", &record, &launch, 1);
    // Keep the job root renameable. Some macOS versions refuse to rename a
    // non-writable directory; this test needs failure after quarantine instead.
    let locked = fixture.state.join("000000000001/locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("keep"), b"force a partial deletion").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
    let output = fixture.command(&["prune"]).output().unwrap();
    assert!(!output.status.success());
    assert!(
        !fixture.state.join("000000000001").exists(),
        "expected quarantine after deletion fault: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pending = fs::read_dir(&fixture.state)
        .unwrap()
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".prune-000000000001-")
        })
        .unwrap()
        .path();
    // A partial delete must not be mistaken for an unreadable launch record.
    let next = fixture.call(&[
        "run",
        "--key",
        "after-collection-fault",
        "--grace",
        "100ms",
        "--",
        "/bin/sleep",
        "30",
    ]);
    assert_eq!(next["job"]["state"], "running");
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(pending.join("locked"), fs::Permissions::from_mode(0o700)).unwrap();
    let report = fixture.call(&["prune"]);
    assert!(report["removed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "000000000001"));
    assert!(!pending.exists());
}

#[test]
fn teardown_waits_for_keeper_finalization_before_removing_state() {
    let fixture = Fixture::new();
    let launched = fixture.call(&[
        "run",
        "--ttl",
        "30s",
        "--grace",
        "2s",
        "--",
        "/bin/sleep",
        "30",
    ]);
    let id = launched["job"]["id"].as_str().unwrap().to_owned();
    let keeper = launched["job"]["keeper_pid"].as_u64().unwrap() as u32;
    let guardian = launched["job"]["guardian_pid"].as_u64().unwrap() as i32;
    // Race-free identification proof: with a 30s lease, both supervisors of
    // this fixture's own state root are running and must be found by argv.
    let live = fixture.supervisors();
    assert!(
        live.contains(&keeper) && live.contains(&(guardian as u32)),
        "keeper {keeper} and guardian {guardian} must be identified (live {live:?})"
    );
    // After a guardian crash the keeper terminates the group, then waits out
    // the grace before its final record write, socket removal and collection.
    assert_eq!(unsafe { libc::kill(guardian, libc::SIGKILL) }, 0);
    assert_eq!(fixture.terminal(&id)["state"], "interrupted");
    let on_disk = || -> Value {
        serde_json::from_slice(&fs::read(fixture.state.join(&id).join("record.json")).unwrap())
            .unwrap()
    };
    if on_disk()["state"] == "stopping" {
        // The finished state was derived from an absent group (on macOS a
        // retained zombie leader is not a live member); the keeper has not
        // finalized and still writes into this state. It may legitimately
        // finish its grace between that snapshot and the scan, so revalidate
        // afterwards: a record still `stopping` after a scan that missed the
        // keeper proves it was pre-final, hence alive, and was not identified.
        let identified = fixture.supervisors().contains(&keeper);
        assert!(
            identified || on_disk()["state"] != "stopping",
            "keeper {keeper} must be identified while it owns pending writes"
        );
    }
    let root = fixture.dir.path().to_owned();
    let state_root = fs::canonicalize(&fixture.state).unwrap();
    drop(fixture);
    // The keeper may be absent, or exited but not yet reaped (a zombie, e.g.
    // under a Linux subreaper); either way it can no longer write. Only a
    // still-running supervisor of this fixture's state root fails, and a PID
    // recycled by an unrelated process does not match its argv.
    let live = supervisors_of(&state_root);
    assert!(
        live.is_empty(),
        "teardown returned while keeper {keeper} was still running (live supervisors {live:?})"
    );
    assert!(
        !root.exists(),
        "fixture state must be removed only after its keeper exits"
    );
}

#[test]
fn explicit_extension_and_observation_have_distinct_semantics() {
    let fixture = Fixture::new();
    let (id, path) = fixture.start("normal", "60s");
    let info = ready(&path);
    let before = fixture.job(&id)["expires_at_ms"].as_u64().unwrap();
    fixture.call(&["logs", &id]);
    fixture.call(&["watch", "--once"]);
    assert_eq!(fixture.job(&id)["expires_at_ms"], before);
    let extended = fixture.call(&["extend", &id, "--by", "1s"]);
    assert_eq!(extended["job"]["expires_at_ms"], before + 1000);
    assert_eq!(fixture.call(&["stop", &id])["job"]["state"], "stopped");
    assert_eq!(fixture.call(&["stop", &id])["job"]["state"], "stopped");
    assert!(!fixture
        .command(&["extend", &id, "--by", "1s"])
        .output()
        .unwrap()
        .status
        .success());
    assert_port_closed(&info);
}

#[test]
fn keeper_failure_is_fail_closed() {
    let fixture = Fixture::new();
    let (id, path) = fixture.start("stubborn", "30s");
    let info = ready(&path);
    let job = fixture.job(&id);
    assert_eq!(
        unsafe { libc::kill(job["keeper_pid"].as_u64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    assert_eq!(fixture.terminal(&id)["state"], "interrupted");
    assert_eq!(fixture.call(&["stop", &id])["job"]["state"], "interrupted");
    assert_port_closed(&info);
}

#[test]
fn guardian_failure_is_cleaned_by_keeper() {
    let fixture = Fixture::new();
    let (id, path) = fixture.start("stubborn", "30s");
    let info = ready(&path);
    let job = fixture.job(&id);
    assert_eq!(
        unsafe { libc::kill(job["guardian_pid"].as_u64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    assert_eq!(fixture.terminal(&id)["state"], "interrupted");
    assert_port_closed(&info);
}

#[test]
fn leader_exit_does_not_leave_a_listening_descendant() {
    let fixture = Fixture::new();
    let (id, path) = fixture.start("leader-exits", "30s");
    let info = ready(&path);
    assert_eq!(fixture.terminal(&id)["state"], "exited");
    assert_port_closed(&info);
}

#[test]
fn capture_is_bounded_and_terminal_controls_are_escaped() {
    let fixture = Fixture::new();
    let ready_path = fixture.dir.path().join("ready.json");
    let service = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/service.py");
    let result = fixture.call(&[
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--log-limit",
        "1024",
        "--",
        "/usr/bin/python3",
        service.to_str().unwrap(),
        "flood",
        ready_path.to_str().unwrap(),
    ]);
    let id = result["job"]["id"].as_str().unwrap();
    ready(&ready_path);
    wait(|| {
        if fixture.job(id)["logs_truncated"] == true {
            Some(())
        } else {
            None
        }
    });
    assert!(
        fs::metadata(fixture.state.join(id).join("output.log"))
            .unwrap()
            .len()
            <= 1024
    );
    let logs = fixture.call(&["logs", id]);
    assert!(!logs["lines"].to_string().contains('\u{1b}'));
    assert!(logs["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains("\\u{1b}")));
}

#[test]
fn explicit_retry_is_idempotent_without_renewal_or_resurrection() {
    let fixture = Fixture::new();
    let args = [
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--key",
        "retry-1",
        "--",
        "/bin/sleep",
        "30",
    ];
    let first = fixture.call(&args);
    let second = fixture.call(&args);
    assert_eq!(first["job"]["id"], second["job"]["id"]);
    assert_eq!(
        first["job"]["expires_at_ms"],
        second["job"]["expires_at_ms"]
    );
    assert_eq!(second["reused"], true);
    let mut different_policy = args;
    different_policy[2] = "1s";
    assert!(!fixture
        .command(&different_policy)
        .output()
        .unwrap()
        .status
        .success());
    let output = fixture
        .command(&args)
        .env("PL_MARKER", "different-env")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let id = first["job"]["id"].as_str().unwrap();
    fixture.call(&["stop", id]);
    assert_eq!(fixture.call(&args)["job"]["state"], "stopped");
    assert_eq!(
        fixture.call(&["list", "--all"])["jobs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn concurrent_retries_create_one_job() {
    let fixture = Fixture::new();
    let state = fixture.state.clone();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let state = state.clone();
            thread::spawn(move || {
                let output = Command::new(env!("CARGO_BIN_EXE_proclease"))
                    .arg("--state-dir")
                    .arg(state)
                    .args([
                        "--json",
                        "run",
                        "--ttl",
                        "30s",
                        "--grace",
                        "100ms",
                        "--key",
                        "concurrent",
                        "--",
                        "/bin/sleep",
                        "30",
                    ])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stdout)
                );
                parse(&output)["job"]["id"].clone()
            })
        })
        .collect();
    let ids: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(fixture.call(&["list"])["jobs"].as_array().unwrap().len(), 1);
}

#[test]
fn failed_launch_and_invalid_inputs_are_explicit() {
    let fixture = Fixture::new();
    let output = fixture
        .command(&["run", "--", "/proclease-does-not-exist"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(parse(&output)["job"]["state"], "failed");
    for args in [
        vec!["stop", "../not-a-job"],
        vec!["run", "--ttl", "0s", "--", "/bin/sleep", "1"],
        vec!["run", "--ttl", "25h", "--", "/bin/sleep", "1"],
    ] {
        assert!(!fixture.command(&args).output().unwrap().status.success());
    }
}

#[test]
fn private_state_and_environment_inheritance() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let ready_path = fixture.dir.path().join("ready.json");
    let service = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/service.py");
    let output = fixture
        .command(&[
            "run",
            "--ttl",
            "30s",
            "--grace",
            "100ms",
            "--",
            "/usr/bin/python3",
            service.to_str().unwrap(),
            "normal",
            ready_path.to_str().unwrap(),
        ])
        .env("PL_MARKER", "inherited-test-value")
        .current_dir(fixture.dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let record = parse(&output);
    let id = record["job"]["id"].as_str().unwrap();
    let info = ready(&ready_path);
    assert_eq!(info["marker"], "inherited-test-value");
    assert_eq!(
        PathBuf::from(info["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        fixture.dir.path().canonicalize().unwrap()
    );
    assert_eq!(
        fs::metadata(&fixture.state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for name in ["launch.json", "record.json", "output.log", "capture.status"] {
        let path = fixture.state.join(id).join(name);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!fs::read_to_string(path)
            .unwrap()
            .contains("inherited-test-value"));
    }
    fixture.call(&["stop", id]);
    fs::set_permissions(&fixture.state, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!fixture
        .command(&["list"])
        .output()
        .unwrap()
        .status
        .success());
    fs::set_permissions(&fixture.state, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn stale_state_never_grants_authority_to_kill_an_unrelated_pid() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new();
    let result = fixture.call(&["run", "--grace", "100ms", "--", "/usr/bin/true"]);
    let id = result["job"]["id"].as_str().unwrap();
    fixture.terminal(id);
    // Wait for the keeper's persisted terminal record before intentionally
    // corrupting it; a derived observation can precede the keeper's last write.
    wait(|| {
        let bytes = fs::read(fixture.state.join(id).join("record.json")).ok()?;
        let record: Value = serde_json::from_slice(&bytes).ok()?;
        if record["state"] == "exited" {
            Some(())
        } else {
            None
        }
    });
    let mut unrelated = OwnedChild(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    let path = fixture.state.join(id).join("record.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["state"] = serde_json::json!("running");
    record["created_at_ms"] = serde_json::json!(0);
    record["guardian_pid"] = serde_json::json!(unrelated.0.id());
    record["command_pid"] = serde_json::json!(unrelated.0.id());
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(fixture.job(id)["state"], "unknown");
    assert!(!fixture
        .command(&["stop", id])
        .output()
        .unwrap()
        .status
        .success());
    assert!(unrelated.0.try_wait().unwrap().is_none());
}

#[test]
fn slow_partial_control_request_cannot_starve_expiry() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let fixture = Fixture::new();
    // Only the guardian's control socket is needed; interpreter startup must
    // not consume this deliberately short lease before the client connects.
    let result = fixture.call(&[
        "run",
        "--ttl",
        "700ms",
        "--grace",
        "100ms",
        "--",
        "/bin/sleep",
        "30",
    ]);
    let id = result["job"]["id"].as_str().unwrap().to_owned();
    let socket = fixture.state.join(&id).join("ctl.sock");
    let writer = thread::spawn(move || {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if stream.write_all(b" ").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    let start = Instant::now();
    assert_eq!(fixture.terminal(&id)["state"], "expired");
    let elapsed = start.elapsed();
    writer.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(1500),
        "slow client delayed expiry by {elapsed:?}"
    );
}

#[test]
fn metadata_write_failure_does_not_abandon_service_group() {
    use std::os::unix::fs::PermissionsExt;
    struct RestorePermissions(PathBuf);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    // Root bypasses this permission fault. CI runs the suite as an ordinary user.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let fixture = Fixture::new();
    let ready_path = fixture.dir.path().join("fault.json");
    let service = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/service.py");
    let result = fixture.call(&[
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--log-limit",
        "1",
        "--",
        "/usr/bin/python3",
        service.to_str().unwrap(),
        "gated-flood",
        ready_path.to_str().unwrap(),
    ]);
    let id = result["job"]["id"].as_str().unwrap();
    let info = ready(&ready_path);
    let dir = fixture.state.join(id);
    let _restore = RestorePermissions(dir.clone());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    // Release output only after installing the fault, rather than guessing
    // how quickly the interpreter and this test will be scheduled.
    fs::write(ready_path.with_extension("release"), b"go").unwrap();
    assert_port_closed(&info);
    // The guardian's cleanup runs despite its failed save, and the keeper's
    // cleanup guard does not depend on writing into the same broken directory.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let (second_id, second_path) = fixture.start("stubborn", "5s");
    let second_info = ready(&second_path);
    let job = fixture.job(&second_id);
    let record_path = fixture.state.join(&second_id).join("record.json");
    fs::set_permissions(&record_path, fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        unsafe { libc::kill(job["guardian_pid"].as_u64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    assert_port_closed(&second_info);
    fs::set_permissions(record_path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn shutdown_output_truncation_is_reported_in_terminal_state() {
    let fixture = Fixture::new();
    let ready_path = fixture.dir.path().join("shutdown.json");
    let service = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/service.py");
    let result = fixture.call(&[
        "run",
        "--ttl",
        "20s",
        "--grace",
        "200ms",
        "--log-limit",
        "1",
        "--",
        "/usr/bin/python3",
        service.to_str().unwrap(),
        "shutdown-output",
        ready_path.to_str().unwrap(),
    ]);
    let id = result["job"]["id"].as_str().unwrap();
    ready(&ready_path);
    assert_eq!(fixture.job(id)["logs_truncated"], false);
    assert_eq!(fixture.call(&["stop", id])["job"]["logs_truncated"], true);
    assert_eq!(fixture.call(&["logs", id])["logs_truncated"], true);
    assert_eq!(
        fs::metadata(fixture.state.join(id).join("output.log"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn grace_window_controls_return_lifecycle_results() {
    let fixture = Fixture::new();
    let result = fixture.call(&[
        "run",
        "--ttl",
        "20s",
        "--grace",
        "700ms",
        "--",
        "/bin/sleep",
        "20",
    ]);
    let id = result["job"]["id"].as_str().unwrap().to_owned();
    let mut first_stop = fixture.command(&["stop", &id]);
    let first = thread::spawn(move || first_stop.output().unwrap());
    wait(|| {
        if fixture.job(&id)["state"] == "stopping" {
            Some(())
        } else {
            None
        }
    });
    let extend = fixture
        .command(&["extend", &id, "--by", "1s"])
        .output()
        .unwrap();
    assert!(!extend.status.success());
    assert!(parse(&extend)["error"]
        .as_str()
        .unwrap()
        .contains("already stopping"));
    assert_eq!(fixture.call(&["stop", &id])["job"]["state"], "stopped");
    let first = first.join().unwrap();
    assert!(first.status.success());
    assert_eq!(parse(&first)["job"]["state"], "stopped");
}

#[test]
fn empty_launch_reservation_does_not_block_keyed_runs() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fixture.call(&["list"]);
    let empty = fixture.state.join("0123456789ab");
    fs::create_dir(&empty).unwrap();
    fs::set_permissions(&empty, fs::Permissions::from_mode(0o700)).unwrap();
    let args = [
        "run",
        "--ttl",
        "20s",
        "--grace",
        "100ms",
        "--key",
        "orphan-safe",
        "--",
        "/bin/sleep",
        "20",
    ];
    let first = fixture.call(&args);
    let second = fixture.call(&args);
    assert_eq!(first["job"]["id"], second["job"]["id"]);
    assert_eq!(second["reused"], true);
    assert_eq!(fixture.call(&["list"])["warnings"], serde_json::json!([]));
    assert!(
        empty.exists(),
        "read-only orphan classification must not delete state"
    );
    // A kill during the first atomic write leaves only a private UUID temp file.
    let pending = empty.join(".0123456789abcdef0123456789abcdef.tmp");
    fs::write(&pending, b"partial launch metadata").unwrap();
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(fixture.call(&args)["job"]["id"], first["job"]["id"]);
    assert_eq!(fixture.call(&["list"])["warnings"], serde_json::json!([]));
    assert!(pending.exists(), "classification must preserve temp files");
    // A directory with a temp-looking name is not an atomic-write artifact.
    fs::remove_file(&pending).unwrap();
    fs::create_dir(&pending).unwrap();
    assert!(!fixture.command(&args).output().unwrap().status.success());
    fs::remove_dir(&pending).unwrap();
    // Nor is a symlink, even if it has exactly the generated temporary name.
    std::os::unix::fs::symlink("missing", &pending).unwrap();
    assert!(!fixture.command(&args).output().unwrap().status.success());
    fs::remove_file(&pending).unwrap();
    // Nonempty unknown state is ambiguous and must still fail closed.
    fs::write(empty.join("unfinished"), b"unknown").unwrap();
    let output = fixture.command(&args).output().unwrap();
    assert!(!output.status.success());
    assert!(parse(&output)["error"]
        .as_str()
        .unwrap()
        .contains("cannot safely resolve"));
}

#[test]
fn retry_context_separates_arguments_from_environment_and_verifies_legacy_records() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let cwd = fixture.dir.path().to_owned();
    let launch_in = |target: &Fixture, args: &[&str], extra: &[(&str, &str)]| {
        let mut command = target.command(args);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .envs(extra.iter().copied())
            .current_dir(&cwd);
        command.output().unwrap()
    };
    let run = |argv: &[&str], extra: &[(&str, &str)]| {
        let mut args = vec!["run", "--grace", "100ms", "--key", "aliased", "--"];
        args.extend_from_slice(argv);
        launch_in(&fixture, &args, extra)
    };
    let rejected = |output: Output| {
        assert!(!output.status.success());
        assert!(parse(&output)["error"]
            .as_str()
            .unwrap()
            .contains("different launch context"));
    };
    // Earlier versions hashed these two contexts to the same bytes.
    let long = ["/usr/bin/true", "A", "value"];
    let short = ["/usr/bin/true"];
    let first = run(&long, &[]);
    assert!(first.status.success());
    let id = parse(&first)["job"]["id"].as_str().unwrap().to_owned();
    fixture.terminal(&id);
    rejected(run(&short, &[("A", "value")]));
    let retry = parse(&run(&long, &[]));
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["job"]["id"], id.as_str());

    // A record written with the earlier encoding remains usable for its exact
    // context, but its ambiguous hash alone does not identify a launch.
    let path = fixture.state.join(&id).join("launch.json");
    let mut launch: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(launch["fingerprint"].as_str().unwrap().starts_with("v2:"));
    let mut digest = Sha256::new();
    digest.update(launch["cwd"].as_str().unwrap().as_bytes());
    digest.update([0]);
    for arg in launch["argv"].as_array().unwrap() {
        digest.update(arg.as_str().unwrap().as_bytes());
        digest.update([0]);
    }
    // The platform can add entries at process start (macOS sets
    // __CF_USER_TEXT_ENCODING), so observe the inherited environment through a
    // separate, isolated job instead of assuming it.
    let probe = Fixture::new();
    let shown = parse(&launch_in(
        &probe,
        &["run", "--grace", "100ms", "--", "/usr/bin/env"],
        &[],
    ));
    let probe_id = shown["job"]["id"].as_str().unwrap();
    assert_eq!(probe.terminal(probe_id)["state"], "exited");
    let observed = fs::read_to_string(probe.state.join(probe_id).join("output.log")).unwrap();
    let mut env: Vec<_> = observed
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| *key != "_" && *key != "SHLVL")
        .collect();
    env.sort();
    assert!(env.contains(&("PATH", "/usr/bin:/bin")), "{observed}");
    for (key, value) in env {
        for part in [key, value] {
            digest.update(part.as_bytes());
            digest.update([0]);
        }
    }
    launch["fingerprint"] = serde_json::json!(format!("{:x}", digest.finalize()));
    fs::write(&path, serde_json::to_vec(&launch).unwrap()).unwrap();
    let legacy = parse(&run(&long, &[]));
    assert_eq!(legacy["reused"], true);
    assert_eq!(legacy["job"]["id"], id.as_str());
    rejected(run(&short, &[("A", "value")]));
    rejected(run(&long, &[("B", "changed")]));
    assert_eq!(
        fixture.call(&["list", "--all"])["jobs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let text = fs::read_to_string(&path).unwrap()
        + &fs::read_to_string(fixture.state.join(&id).join("record.json")).unwrap();
    assert!(
        !text.contains("/usr/bin:/bin"),
        "environment must not be persisted"
    );
}

#[test]
fn overdue_startup_expires_without_starting_the_command() {
    let fixture = Fixture::new();
    let marker = fixture.dir.path().join("late-start");
    let args = [
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--key",
        "late-start",
        "--",
        "/usr/bin/touch",
        marker.to_str().unwrap(),
    ];
    // A generous lease lets the template complete even on a slow host.
    let (record, launch) = fixture.template(&args);
    assert_eq!(record["state"], "exited", "{record}");
    assert_eq!(record["exit_code"], 0, "{record}");
    assert!(marker.exists(), "the template command must have run");
    fs::remove_file(&marker).unwrap();
    // A committed launch whose supervisor starts after the persisted deadline,
    // as after a slow or suspended startup, without depending on timing: the
    // seeded 30s lease began 60s ago.
    let id = "0000000000e1";
    fixture.seed_starting(id, &record, &launch, now_ms() - 60_000);
    let (output, record) = fixture.keep(id);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(record["state"], "expired");
    assert_eq!(record["reason"], "deadline");
    assert!(record["command_pid"].is_null());
    assert!(record["guardian_pid"].is_u64());
    assert!(record["finished_at_ms"].is_u64());
    assert!(
        !marker.exists(),
        "an overdue lease must not run its command"
    );
    let retry = fixture.call(&args);
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["job"]["id"], id);
    assert_eq!(retry["job"]["state"], "expired");
    assert!(!marker.exists());
}

#[test]
fn guardian_setup_failure_is_a_failed_launch() {
    let fixture = Fixture::new();
    let marker = fixture.dir.path().join("setup-fault");
    let args = [
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--key",
        "setup-fault",
        "--",
        "/usr/bin/touch",
        marker.to_str().unwrap(),
    ];
    let (record, launch) = fixture.template(&args);
    fs::remove_file(&marker).unwrap();
    let id = "0000000000e2";
    fixture.seed_starting(id, &record, &launch, now_ms());
    // An existing non-socket path makes the guardian's control bind fail
    // before it spawns the command.
    fs::write(fixture.state.join(id).join("ctl.sock"), b"").unwrap();
    let (output, record) = fixture.keep(id);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(record["state"], "failed");
    assert_eq!(record["reason"], "launch_failed");
    assert!(record["command_pid"].is_null());
    assert!(record["error"]
        .as_str()
        .unwrap()
        .contains("cannot bind control socket"));
    let diagnostic = fs::read_to_string(fixture.state.join(id).join("supervisor.log")).unwrap();
    assert!(
        diagnostic.contains("cannot bind control socket"),
        "{diagnostic}"
    );
    assert!(!marker.exists());
    let retry = fixture.command(&args).output().unwrap();
    assert!(
        !retry.status.success(),
        "a failed launch must not report success"
    );
    let retry = parse(&retry);
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["job"]["id"], id);
    assert_eq!(retry["job"]["state"], "failed");
    assert!(!marker.exists());
}

#[test]
fn unspawned_keeper_is_a_failed_collectable_launch_but_uncertain_startup_is_kept() {
    use std::os::unix::process::CommandExt;
    // Root bypasses the process limit used to make the keeper spawn fail.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let fixture = Fixture::new();
    let marker = fixture.dir.path().join("no-keeper");
    let args = [
        "run",
        "--ttl",
        "30s",
        "--grace",
        "100ms",
        "--key",
        "no-keeper",
        "--",
        "/usr/bin/touch",
        marker.to_str().unwrap(),
    ];
    let mut limited = fixture.command(&args);
    // Applied only to the CLI process: this user already has more than one
    // process, so the CLI's keeper fork fails after launch metadata commits.
    unsafe {
        limited.pre_exec(|| {
            let mut limit: libc::rlimit = std::mem::zeroed();
            if libc::getrlimit(libc::RLIMIT_NPROC, &mut limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            limit.rlim_cur = 1;
            if libc::setrlimit(libc::RLIMIT_NPROC, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = limited.output().unwrap();
    assert!(!output.status.success());
    let failed = parse(&output);
    assert_eq!(failed["job"]["state"], "failed", "{failed}");
    assert_eq!(failed["job"]["reason"], "launch_failed");
    assert_eq!(failed["job"]["keeper_pid"], 0);
    assert!(failed["job"]["error"]
        .as_str()
        .unwrap()
        .contains("cannot start supervisor"));
    let id = failed["job"]["id"].as_str().unwrap().to_owned();
    let retry = fixture.command(&args).output().unwrap();
    assert!(
        !retry.status.success(),
        "a failed launch must not be reused as success"
    );
    let retry = parse(&retry);
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["job"]["id"], id.as_str());
    assert_eq!(retry["job"]["state"], "failed");
    assert!(!marker.exists());

    // Uncertain outcomes keep the conservative protection, even without a
    // recorded keeper PID: a startup that never acknowledged, unknown state, a
    // non-launch failure, and a launch failure with a recorded guardian.
    let path = fixture.state.join(&id).join("record.json");
    let record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let launch: Value =
        serde_json::from_slice(&fs::read(fixture.state.join(&id).join("launch.json")).unwrap())
            .unwrap();
    let mut protected = Vec::new();
    for (index, (state, reason, guardian)) in [
        ("starting", Value::Null, Value::Null),
        ("unknown", Value::Null, Value::Null),
        (
            "interrupted",
            serde_json::json!("supervision_lost"),
            Value::Null,
        ),
        (
            "failed",
            serde_json::json!("launch_failed"),
            serde_json::json!(unsafe { libc::getpgrp() }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let seeded = format!("0000000000f{index}");
        let mut uncertain = record.clone();
        uncertain["state"] = serde_json::json!(state);
        uncertain["reason"] = reason;
        uncertain["guardian_pid"] = guardian;
        uncertain["created_at_ms"] = serde_json::json!(0);
        let mut launch = launch.clone();
        launch["key"] = Value::Null;
        fixture.seed_history(&seeded, &uncertain, &launch, 1);
        protected.push(seeded);
    }

    let mut aged = record;
    aged["finished_at_ms"] = serde_json::json!(1);
    fs::write(&path, serde_json::to_vec(&aged).unwrap()).unwrap();
    let preview = fixture.call(&["prune", "--dry-run"]);
    assert_eq!(preview["candidates"], serde_json::json!([id]));
    let report = fixture.call(&["prune"]);
    assert_eq!(report["removed"], serde_json::json!([id]));
    assert!(!fixture.state.join(&id).exists());
    for seeded in protected {
        assert!(
            fixture.state.join(&seeded).exists(),
            "uncertain job {seeded} was collected"
        );
    }
}

impl Fixture {
    /// Start the fixture service with declared labels and a chosen grace.
    fn start_labeled(&self, owner: &str, task: &str, mode: &str, grace: &str) -> (String, Value) {
        let marker = self
            .dir
            .path()
            .join(format!("r-{}.json", uuid::Uuid::new_v4().simple()));
        let service = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/service.py");
        let result = self.call(&[
            "run",
            "--ttl",
            "60s",
            "--grace",
            grace,
            "--owner",
            owner,
            "--task",
            task,
            "--",
            "/usr/bin/python3",
            service.to_str().unwrap(),
            mode,
            marker.to_str().unwrap(),
        ]);
        let id = result["job"]["id"].as_str().unwrap().to_owned();
        (id, ready(&marker))
    }
    fn bulk(&self, args: &[&str]) -> (bool, Value) {
        let mut full = vec!["stop", "--all"];
        full.extend_from_slice(args);
        let output = self.command(&full).output().unwrap();
        (output.status.success(), parse(&output))
    }
}

fn result_ids(value: &Value) -> Vec<String> {
    let mut ids: Vec<String> = value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

fn sorted(ids: &[&String]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    ids.sort();
    ids
}

fn port_open(info: &Value) -> bool {
    let address: SocketAddr = format!("127.0.0.1:{}", info["port"].as_u64().unwrap())
        .parse()
        .unwrap();
    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
}

#[test]
fn stop_requires_exactly_one_target_and_single_id_json_is_unchanged() {
    let fixture = Fixture::new();
    let (id, info) = fixture.start_labeled("a", "t", "normal", "100ms");
    for args in [
        vec!["stop"],
        vec!["stop", "--owner", "a"],
        vec!["stop", "--task", "t"],
        vec!["stop", "--dry-run"],
        vec!["stop", &id, "--all"],
        vec!["stop", &id, "--owner", "a"],
        vec!["stop", &id, "--task", "t"],
        vec!["stop", &id, "--dry-run"],
    ] {
        let output = fixture.command(&args).output().unwrap();
        // Argument errors are usage errors (clap exit 2), not lifecycle failures.
        assert_eq!(output.status.code(), Some(2), "{args:?} must be rejected");
    }
    assert_eq!(fixture.job(&id)["state"], "running");
    assert!(port_open(&info));
    let single = fixture.call(&["stop", &id]);
    let mut keys: Vec<_> = single.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["job", "reused", "schema_version"]);
    assert_eq!(single["schema_version"], 1);
    assert_eq!(single["reused"], false);
    assert_eq!(single["job"]["state"], "stopped");
    assert_port_closed(&info);
}

#[test]
fn bulk_stop_filters_dry_run_and_cleans_stubborn_groups() {
    let fixture = Fixture::new();
    let (a, a_info) = fixture.start_labeled("a", "t1", "stubborn", "100ms");
    let (b, b_info) = fixture.start_labeled("a", "t2", "stubborn", "100ms");
    let (c, c_info) = fixture.start_labeled("b", "t1", "normal", "100ms");
    let deadlines: Vec<_> = [&a, &b, &c]
        .iter()
        .map(|id| fixture.job(id)["expires_at_ms"].clone())
        .collect();

    let (ok, dry) = fixture.bulk(&["--owner", "a", "--task", "t1", "--dry-run"]);
    assert!(ok, "{dry}");
    assert_eq!(dry["schema_version"], 1);
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["complete"], true);
    assert_eq!(dry["filters"]["owner"], "a");
    assert_eq!(result_ids(&dry), sorted(&[&a]));
    assert_eq!(dry["results"][0]["status"], "would_stop");
    assert_eq!(dry["results"][0]["job"]["task"], "t1");
    let (ok, dry_all) = fixture.bulk(&["--dry-run"]);
    assert!(ok);
    assert_eq!(result_ids(&dry_all), sorted(&[&a, &b, &c]));
    for (id, deadline) in [&a, &b, &c].iter().zip(&deadlines) {
        let job = fixture.job(id);
        assert_eq!(job["state"], "running");
        assert_eq!(&job["expires_at_ms"], deadline);
    }
    for info in [&a_info, &b_info, &c_info] {
        assert!(port_open(info));
    }

    // Exact AND: owner "a" with task "t1" selects only A.
    let (ok, scoped) = fixture.bulk(&["--owner", "a", "--task", "t1"]);
    assert!(ok, "{scoped}");
    assert_eq!(scoped["dry_run"], false);
    assert_eq!(result_ids(&scoped), sorted(&[&a]));
    assert_eq!(scoped["results"][0]["status"], "finished");
    assert_eq!(scoped["results"][0]["ok"], true);
    assert_eq!(scoped["results"][0]["selected_state"], "running");
    assert_eq!(scoped["results"][0]["job"]["state"], "stopped");
    assert_port_closed(&a_info);
    for (id, deadline) in [(&b, &deadlines[1]), (&c, &deadlines[2])] {
        let job = fixture.job(id);
        assert_eq!(job["state"], "running");
        assert_eq!(&job["expires_at_ms"], deadline);
    }
    // A label prefix or a non-matching task never widens the selection.
    let (ok, none) = fixture.bulk(&["--owner", "", "--task", "t"]);
    assert!(ok);
    assert!(none["results"].as_array().unwrap().is_empty());

    let (ok, rest) = fixture.bulk(&[]);
    assert!(ok, "{rest}");
    assert_eq!(result_ids(&rest), sorted(&[&b, &c]));
    assert!(rest["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["job"]["state"] == "stopped"));
    assert_port_closed(&b_info);
    assert_port_closed(&c_info);
    let child = b_info["child"].as_u64().unwrap() as i32;
    wait(|| (unsafe { libc::kill(child, 0) } == -1).then_some(()));

    // Repeating is an empty success; finished history is retained.
    let (ok, again) = fixture.bulk(&[]);
    assert!(ok);
    assert_eq!(again["complete"], true);
    assert!(again["results"].as_array().unwrap().is_empty());
    for id in [&a, &b, &c] {
        assert_eq!(fixture.job(id)["state"], "stopped");
    }
}

#[test]
fn bulk_stop_is_parallel_bounded_and_ignores_later_launches() {
    let fixture = Fixture::new();
    let jobs: Vec<_> = (0..4)
        .map(|_| fixture.start_labeled("a", "t", "stubborn", "1500ms"))
        .collect();
    let mut command = fixture.command(&["stop", "--all"]);
    let started = Instant::now();
    let bulk = thread::spawn(move || {
        let output = command.output().unwrap();
        (output, Instant::now())
    });
    // Every selected job reaches stopping before any can finish its grace.
    wait(|| {
        jobs.iter()
            .all(|(id, _)| fixture.job(id)["state"] != "running")
            .then_some(())
    });
    let (late, late_info) = fixture.start_labeled("a", "t", "normal", "100ms");
    let (output, finished) = bulk.join().unwrap();
    let elapsed = finished - started;
    let value = parse(&output);
    assert!(output.status.success(), "{value}");
    let ids: Vec<&String> = jobs.iter().map(|(id, _)| id).collect();
    assert_eq!(result_ids(&value), sorted(&ids));
    assert!(!result_ids(&value).contains(&late));
    // Sequential grace alone would take at least 6s.
    assert!(elapsed >= Duration::from_millis(1400), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(5000), "{elapsed:?}");
    for (_, info) in &jobs {
        assert_port_closed(info);
    }
    assert_eq!(fixture.job(&late)["state"], "running");
    assert!(port_open(&late_info));
}

#[test]
fn bulk_stop_reports_unverified_and_unreadable_records_but_stops_the_rest() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new();
    let (live, live_info) = fixture.start_labeled("a", "t", "normal", "100ms");
    // An unverified job: a finished record rewritten as running, naming an
    // unrelated process owned by this test. Its supervisor is long gone.
    let (record, launch) = fixture.finished_template();
    let unrelated = OwnedChild(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    let mut stale = record.clone();
    stale["state"] = serde_json::json!("running");
    stale["created_at_ms"] = serde_json::json!(0);
    stale["finished_at_ms"] = Value::Null;
    stale["owner"] = serde_json::json!("a");
    stale["task"] = serde_json::json!("t");
    stale["guardian_pid"] = serde_json::json!(unrelated.0.id());
    stale["command_pid"] = serde_json::json!(unrelated.0.id());
    let unknown = "0000000000aa";
    stale["id"] = serde_json::json!(unknown);
    let mut stale_launch = launch.clone();
    stale_launch["id"] = serde_json::json!(unknown);
    fixture.seed_files(
        unknown,
        &[
            ("record.json", serde_json::to_vec(&stale).unwrap()),
            ("launch.json", serde_json::to_vec(&stale_launch).unwrap()),
        ],
    );
    let corrupt = "0000000000bb";
    fixture.seed_files(corrupt, &[("record.json", b"not json".to_vec())]);

    // A filter cannot prove that an unreadable record does not match.
    let (ok, dry) = fixture.bulk(&["--owner", "nobody", "--dry-run"]);
    assert!(!ok);
    assert_eq!(dry["complete"], false);
    assert!(dry["results"].as_array().unwrap().is_empty());
    assert!(dry["warnings"][0].as_str().unwrap().contains(corrupt));
    let (ok, dry) = fixture.bulk(&["--dry-run"]);
    assert!(!ok);
    assert_eq!(result_ids(&dry), sorted(&[&live, &unknown.to_owned()]));
    assert_eq!(fixture.job(&live)["state"], "running");

    let (ok, value) = fixture.bulk(&["--owner", "a"]);
    assert!(!ok, "{value}");
    assert_eq!(value["complete"], false);
    let results = value["results"].as_array().unwrap();
    let by_id = |id: &str| results.iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(by_id(&live)["status"], "finished");
    assert_eq!(by_id(&live)["job"]["state"], "stopped");
    assert_eq!(by_id(unknown)["status"], "unverified");
    assert_eq!(by_id(unknown)["ok"], false);
    assert!(by_id(unknown)["error"]
        .as_str()
        .unwrap()
        .contains("refusing"));
    assert!(value["warnings"][0].as_str().unwrap().contains(corrupt));
    assert_port_closed(&live_info);
    let mut unrelated = unrelated;
    assert!(unrelated.0.try_wait().unwrap().is_none());
    // Teardown must not try to manage the seeded records.
    fs::remove_dir_all(fixture.state.join(unknown)).unwrap();
    fs::remove_dir_all(fixture.state.join(corrupt)).unwrap();
}

#[test]
fn bulk_stop_waits_for_stopping_jobs_and_concurrent_bulk_stops_agree() {
    let fixture = Fixture::new();
    let first = fixture.call(&[
        "run",
        "--ttl",
        "20s",
        "--grace",
        "700ms",
        "--",
        "/bin/sleep",
        "20",
    ])["job"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (second, _) = fixture.start_labeled("a", "t", "stubborn", "700ms");
    let mut single = fixture.command(&["stop", &first]);
    let single = thread::spawn(move || single.output().unwrap());
    wait(|| (fixture.job(&first)["state"] == "stopping").then_some(()));
    let mut other = fixture.command(&["stop", "--all"]);
    let other = thread::spawn(move || other.output().unwrap());
    let (ok, value) = fixture.bulk(&[]);
    assert!(ok, "{value}");
    let other = other.join().unwrap();
    assert!(other.status.success(), "{}", parse(&other));
    for value in [value, parse(&other)] {
        for result in value["results"].as_array().unwrap() {
            assert_eq!(result["status"], "finished");
            assert_eq!(result["job"]["state"], "stopped");
        }
    }
    assert!(single.join().unwrap().status.success());
    assert_eq!(fixture.job(&first)["state"], "stopped");
    assert_eq!(fixture.job(&second)["state"], "stopped");
}

/// Restores a fixture file on drop, so teardown sees valid state even when an
/// assertion fails while the file is deliberately corrupted.
struct Restore(PathBuf, Vec<u8>);
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, &self.1);
    }
}

#[test]
fn record_naming_another_job_cannot_redirect_bulk_stop() {
    let fixture = Fixture::new();
    let (a, a_info) = fixture.start_labeled("o", "A", "normal", "100ms");
    let (b, b_info) = fixture.start_labeled("o", "B", "normal", "100ms");
    let b_deadline = fixture.job(&b)["expires_at_ms"].clone();
    let path = fixture.state.join(&a).join("record.json");
    let original = fs::read(&path).unwrap();
    let restore = Restore(path.clone(), original.clone());
    let mut corrupted: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(corrupted["task"], "A");
    corrupted["id"] = serde_json::json!(b);
    fs::write(&path, serde_json::to_vec(&corrupted).unwrap()).unwrap();

    let (ok, value) = fixture.bulk(&["--task", "A"]);
    assert!(!ok, "{value}");
    assert_eq!(value["complete"], false);
    assert!(value["results"].as_array().unwrap().is_empty(), "{value}");
    let warning = value["warnings"][0].as_str().unwrap();
    assert!(warning.starts_with(&a) && warning.contains("different job"));
    // An unfiltered view selects B once, from its own record, never via A.
    let (ok, dry) = fixture.bulk(&["--dry-run"]);
    assert!(!ok);
    assert_eq!(result_ids(&dry), sorted(&[&b]));
    let job = fixture.job(&b);
    assert_eq!(job["state"], "running");
    assert_eq!(job["expires_at_ms"], b_deadline);
    assert!(port_open(&b_info));
    assert!(port_open(&a_info));
    drop(restore);
    assert_eq!(fixture.job(&a)["state"], "running");
}

/// A test-owned endpoint at a seeded job's control socket. It accepts every
/// connection and trickles one byte at a time without ever ending a reply.
struct Trickler {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    dir: PathBuf,
}

impl Trickler {
    fn new(dir: PathBuf) -> Self {
        use std::{io::Write, os::unix::net::UnixListener, sync::atomic::Ordering};
        let listener = UnixListener::bind(dir.join("ctl.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            let mut streams = Vec::new();
            while !flag.load(Ordering::Relaxed) {
                if let Ok((stream, _)) = listener.accept() {
                    stream.set_nonblocking(true).unwrap();
                    streams.push(stream);
                }
                streams.retain_mut(|stream| match stream.write(b" ") {
                    Ok(_) => true,
                    Err(error) => error.kind() == std::io::ErrorKind::WouldBlock,
                });
                thread::sleep(Duration::from_millis(20));
            }
        });
        Self {
            stop,
            thread: Some(thread),
            dir,
        }
    }
}

impl Drop for Trickler {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Teardown must not try to manage the seeded record.
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn trickling_control_endpoint_is_bounded_while_healthy_jobs_stop() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = Fixture::new();
    let (healthy, healthy_info) = fixture.start_labeled("a", "trickle", "stubborn", "100ms");
    let (record, launch) = fixture.finished_template();
    let unrelated = OwnedChild(Command::new("/bin/sleep").arg("30").spawn().unwrap());
    let fake = "0000000000cc";
    let mut seeded = record.clone();
    for (field, value) in [
        ("id", serde_json::json!(fake)),
        ("state", serde_json::json!("running")),
        ("created_at_ms", serde_json::json!(now_ms())),
        ("expires_at_ms", serde_json::json!(now_ms() + 60_000)),
        ("finished_at_ms", Value::Null),
        ("reason", Value::Null),
        ("owner", serde_json::json!("a")),
        ("task", serde_json::json!("trickle")),
        ("guardian_pid", serde_json::json!(unrelated.0.id())),
        ("command_pid", serde_json::json!(unrelated.0.id())),
    ] {
        seeded[field] = value;
    }
    let mut seeded_launch = launch.clone();
    seeded_launch["id"] = serde_json::json!(fake);
    fixture.seed_files(
        fake,
        &[
            ("record.json", serde_json::to_vec(&seeded).unwrap()),
            ("launch.json", serde_json::to_vec(&seeded_launch).unwrap()),
        ],
    );
    let _endpoint = Trickler::new(fixture.state.join(fake));

    let started = Instant::now();
    let (ok, value) = fixture.bulk(&["--task", "trickle"]);
    let elapsed = started.elapsed();
    assert!(!ok, "{value}");
    assert_eq!(value["complete"], false);
    // Two Stop attempts, each with an absolute 2s exchange bound.
    assert!(elapsed >= Duration::from_millis(3_900), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(7_000), "{elapsed:?}");
    let results = value["results"].as_array().unwrap();
    let by_id = |id: &str| results.iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(by_id(&healthy)["status"], "finished");
    assert_eq!(by_id(&healthy)["job"]["state"], "stopped");
    assert_eq!(by_id(fake)["status"], "failed");
    assert!(by_id(fake)["error"].as_str().unwrap().contains("2s bound"));
    assert_port_closed(&healthy_info);

    // A single-ID stop is bounded by the same exchange limit.
    let started = Instant::now();
    let output = fixture.command(&["stop", fake]).output().unwrap();
    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_millis(4_000));
    let mut unrelated = unrelated;
    assert!(unrelated.0.try_wait().unwrap().is_none());
}

#[test]
fn altered_launch_grace_or_metadata_cannot_abort_other_stops() {
    let fixture = Fixture::new();
    let (huge, huge_info) = fixture.start_labeled("g", "grace", "normal", "100ms");
    let (broken, broken_info) = fixture.start_labeled("g", "grace", "normal", "100ms");
    let (normal, normal_info) = fixture.start_labeled("g", "grace", "normal", "100ms");
    let (single, single_info) = fixture.start_labeled("g", "single", "normal", "100ms");
    let set_huge_grace = |id: &str| {
        let path = fixture.state.join(id).join("launch.json");
        let mut launch: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        launch["grace_ms"] = serde_json::json!(u64::MAX);
        fs::write(&path, serde_json::to_vec(&launch).unwrap()).unwrap();
    };
    set_huge_grace(&huge);
    set_huge_grace(&single);
    fs::write(fixture.state.join(&broken).join("launch.json"), b"not json").unwrap();

    let (ok, value) = fixture.bulk(&["--task", "grace"]);
    assert!(ok, "{value}");
    assert_eq!(value["complete"], true);
    assert_eq!(result_ids(&value), sorted(&[&huge, &broken, &normal]));
    for result in value["results"].as_array().unwrap() {
        assert_eq!(result["status"], "finished");
        assert_eq!(result["job"]["state"], "stopped");
    }
    for info in [&huge_info, &broken_info, &normal_info] {
        assert_port_closed(info);
    }
    assert!(port_open(&single_info));
    let stopped = fixture.call(&["stop", &single]);
    assert_eq!(stopped["job"]["state"], "stopped");
    assert_port_closed(&single_info);
}

#[test]
fn bulk_human_output_summarizes_and_escapes_failures() {
    let fixture = Fixture::new();
    let (live, live_info) = fixture.start_labeled("h", "human", "normal", "100ms");
    let (record, launch) = fixture.finished_template();
    let unknown = "0000000000dd";
    let mut stale = record.clone();
    for (field, value) in [
        ("id", serde_json::json!(unknown)),
        ("state", serde_json::json!("running")),
        ("created_at_ms", serde_json::json!(0)),
        ("finished_at_ms", Value::Null),
        ("owner", serde_json::json!("h")),
        ("task", serde_json::json!("human")),
        ("name", serde_json::json!("x\u{1b}[31my\u{7}")),
        ("guardian_pid", Value::Null),
        ("command_pid", Value::Null),
    ] {
        stale[field] = value;
    }
    let mut stale_launch = launch.clone();
    stale_launch["id"] = serde_json::json!(unknown);
    fixture.seed_files(
        unknown,
        &[
            ("record.json", serde_json::to_vec(&stale).unwrap()),
            ("launch.json", serde_json::to_vec(&stale_launch).unwrap()),
        ],
    );
    let corrupt = "0000000000de";
    fixture.seed_files(corrupt, &[("record.json", b"not json".to_vec())]);
    let output = Command::new(env!("CARGO_BIN_EXE_proclease"))
        .arg("--state-dir")
        .arg(&fixture.state)
        .args(["stop", "--all", "--task", "human"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(output.status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(stdout.contains("Stopped 1 of 2 selected jobs."), "{stdout}");
    assert!(
        stderr.contains(&format!(
            "{unknown}: unverified: supervisor unavailable; refusing"
        )),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("Warning: {corrupt}:")), "{stderr}");
    for text in [&stdout, &stderr] {
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{text:?}"
        );
    }
    assert_eq!(fixture.job(&live)["state"], "stopped");
    assert_port_closed(&live_info);
    fs::remove_dir_all(fixture.state.join(unknown)).unwrap();
    fs::remove_dir_all(fixture.state.join(corrupt)).unwrap();
}
