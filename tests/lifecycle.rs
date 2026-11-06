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
    }
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
    let pid = result["job"]["command_pid"].as_u64().unwrap() as i32;
    let job = fixture.terminal(id);
    assert_eq!(job["state"], "expired");
    assert_eq!(job["reason"], "deadline");
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
fn default_views_hide_finished_history_but_keep_unverified_jobs() {
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
        assert_eq!(fixture.call(view)["jobs"], serde_json::json!([]));
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
    // Unavailable supervision is not evidence of completion. Keep this state
    // visible even though routine finished history is hidden.
    record["state"] = serde_json::json!("running");
    fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    for view in [&["list"][..], &["watch", "--once"][..]] {
        assert_eq!(fixture.call(view)["jobs"][0]["state"], "unknown");
    }
}

#[test]
fn explicit_extension_and_observation_have_distinct_semantics() {
    let fixture = Fixture::new();
    let (id, path) = fixture.start("normal", "10s");
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
