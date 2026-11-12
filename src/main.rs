#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("ProcLease targets macOS and Linux.");

mod display;
mod filectl;
mod lifecycle;
mod model;
mod platform;
mod readiness;
mod resources;
mod store;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use model::{
    duration_ms, now_ms, Control, Launch, Mode, Record, State, MAX_LIFETIME_MS, SCHEMA_VERSION,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::{IsTerminal, Read},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use store::{atomic_json, Store};

#[derive(Parser)]
#[command(
    name = "proclease",
    version,
    about = "Put an explicit lifetime on development processes"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        value_enum,
        default_value = "auto",
        help = "Color for human-readable output"
    )]
    color: display::ColorChoice,
    #[arg(long, global = true, help = "Emit versioned JSON")]
    json: bool,
    #[arg(
        long,
        global = true,
        env = "PROCLEASE_HOME",
        help = "Private state directory (absolute path)"
    )]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch a foreground command under an explicit lease.
    Run {
        #[arg(long, default_value = "2h")]
        ttl: String,
        #[arg(
            long,
            default_value = "user",
            help = "Declared agent/caller label; not authenticated identity"
        )]
        owner: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, help = "Expected preview URL; readiness is checked separately")]
        url: Option<String>,
        #[arg(
            long,
            help = "Explicit retry key; a retry must match the original command, directory, \
                    environment and lease policy, or it is rejected (see --reuse)"
        )]
        key: Option<String>,
        #[arg(
            long,
            requires = "key",
            help = "With --key: return the retained job for this key and never launch. \
                    Command, directory and lease policy must still match; the job keeps \
                    its original launch environment, which is not compared"
        )]
        reuse: bool,
        #[arg(
            long,
            help = "Hold this invocation until the job finishes, using private-file control \
                    instead of a Unix socket. Ending this invocation stops the job; run it \
                    where the session can stay open (for example an agent's background task). \
                    A keyed retry or --reuse returns immediately and never attaches"
        )]
        foreground: bool,
        #[arg(long, default_value = "2s")]
        grace: String,
        #[arg(
            long,
            value_name = "PORT",
            help = "After launch, wait until a TCP connection to this loopback port succeeds \
                    (observation only; not an ownership or health check)"
        )]
        wait_for_port: Option<u16>,
        #[arg(
            long,
            value_name = "TEXT",
            help = "After launch, wait until captured output contains this literal text"
        )]
        wait_for_log: Option<String>,
        #[arg(
            long,
            value_name = "DURATION",
            help = "Readiness wait budget, from when waiting starts (default 30s, max 5m); \
                    never changes the lease"
        )]
        wait_timeout: Option<String>,
        #[arg(
            long,
            default_value_t = 1_048_576,
            help = "Combined stdout/stderr capture limit, in bytes (max 16 MiB)"
        )]
        log_limit: u64,
        #[arg(
            required = true,
            last = true,
            help = "Executable and arguments, after --; no implicit shell"
        )]
        command: Vec<String>,
    },
    /// List active, unverified and briefly recent jobs; never renew a lease.
    List {
        #[arg(long, help = "Include finished jobs from retained history")]
        all: bool,
        #[arg(long, help = "Include sampled CPU/RSS metrics in JSON output")]
        resources: bool,
    },
    /// Remove confirmed finished history older than 24h or beyond the newest 100.
    Prune {
        #[arg(long, help = "Show what would be removed without deleting anything")]
        dry_run: bool,
    },
    /// Print bounded, terminal-safe output.
    Logs {
        id: String,
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
    /// Explicitly add a duration to an unexpired lease (total maximum 24h).
    Extend {
        id: String,
        #[arg(long)]
        by: String,
    },
    /// Stop one managed job, or explicitly every selected active job, and its
    /// supported process group.
    Stop {
        #[arg(
            required_unless_present = "all",
            conflicts_with_all = ["all", "owner", "task", "dry_run"]
        )]
        id: Option<String>,
        #[arg(
            long,
            help = "Stop every active job in this state directory matching all filters"
        )]
        all: bool,
        #[arg(
            long,
            requires = "all",
            help = "With --all: exact declared owner label (not authorization)"
        )]
        owner: Option<String>,
        #[arg(long, requires = "all", help = "With --all: exact task label")]
        task: Option<String>,
        #[arg(
            long,
            requires = "all",
            help = "With --all: show the selection without stopping anything"
        )]
        dry_run: bool,
    },
    /// Show the live process table; Ctrl+C closes the view, not the jobs.
    Watch {
        #[arg(long, help = "Include finished jobs from retained history")]
        all: bool,
        #[arg(long, help = "Include sampled CPU/RSS metrics in JSON output")]
        resources: bool,
        #[arg(long, default_value = "1s")]
        interval: String,
        #[arg(long)]
        once: bool,
    },
    #[command(name = "__keeper", hide = true)]
    Keeper { id: String },
    #[command(name = "__guard", hide = true)]
    Guard { id: String },
}

fn default_state() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("proclease"));
    }
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("set PROCLEASE_HOME when HOME is unavailable")?,
    )
    .join(".local/state/proclease"))
}

fn clean(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn cell(value: &str) -> String {
    clean(value).replace(['\n', '\t'], " ")
}

fn view(record: &Record) -> serde_json::Value {
    let mut value = serde_json::to_value(record).expect("record serialization is infallible");
    value["expires_at"] = json!(record.expires_at());
    value
}

fn job_view(
    record: &Record,
    usage: &resources::UsageMap,
    include_resources: bool,
) -> serde_json::Value {
    let mut value = view(record);
    if include_resources {
        value["resources"] = json!(usage.get(&record.id));
    }
    value
}

fn print_record(
    record: &Record,
    reused: bool,
    json_output: bool,
    color: display::ColorChoice,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            json!({"schema_version": SCHEMA_VERSION, "job": view(record), "reused": reused})
        );
    } else {
        display::print_table(
            std::slice::from_ref(record),
            true,
            color,
            &resources::UsageMap::new(),
        )?;
        if reused {
            println!("Existing lease returned; its deadline was not extended.");
        }
    }
    Ok(())
}

/// Wait for readiness outside the launch lock and report it with the job.
/// Exit status 1 unless every criterion was observed while the job ran.
fn print_readiness(
    store: &Store,
    record: Record,
    reused: bool,
    spec: &readiness::Spec,
    json_output: bool,
    color: display::ColorChoice,
) -> Result<i32> {
    let outcome = wait_readiness(store, record, spec, json_output);
    report_readiness(&outcome, reused, spec, json_output, color)
}

fn wait_readiness(
    store: &Store,
    record: Record,
    spec: &readiness::Spec,
    json_output: bool,
) -> readiness::Outcome {
    let id = record.id.clone();
    if !json_output {
        eprintln!(
            "Waiting up to {} for readiness of job {id}; the lease is unchanged.",
            humantime::format_duration(spec.timeout)
        );
    }
    readiness::wait(store, &id, spec, record)
}

fn report_readiness(
    outcome: &readiness::Outcome,
    reused: bool,
    spec: &readiness::Spec,
    json_output: bool,
    color: display::ColorChoice,
) -> Result<i32> {
    let record = &outcome.record;
    let id = record.id.clone();
    let ready = outcome.ready();
    if json_output {
        let mut value = json!({"schema_version": SCHEMA_VERSION, "job": view(record),
            "reused": reused, "readiness": outcome.view(spec)});
        if !ready {
            value["error"] = json!(format!(
                "readiness not confirmed: {}",
                outcome.status.as_str()
            ));
        }
        println!("{value}");
        return Ok(if ready { 0 } else { 1 });
    }
    print_record(record, reused, false, color)?;
    let waited = format!("{:.1}s", outcome.waited.as_secs_f64());
    let mut details = Vec::new();
    if let Some(port) = spec.port {
        details.push(match outcome.port_address {
            Some(address) => format!("loopback port {port} accepted a TCP connection ({address})"),
            None => format!("loopback port {port} did not accept a TCP connection"),
        });
    }
    if spec.log.is_some() {
        details.push(if outcome.log_seen {
            "captured output contained the requested text".to_owned()
        } else if record.logs_truncated {
            "requested text not seen; capture reached its byte cap".to_owned()
        } else {
            "requested text not seen in captured output".to_owned()
        });
    }
    if ready {
        println!("Ready after {waited}: {}.", details.join("; "));
        return Ok(0);
    }
    eprintln!(
        "Readiness not confirmed ({}) after {waited}: {}.",
        outcome.status.as_str(),
        details.join("; ")
    );
    if let Some(error) = &outcome.error {
        eprintln!("Error: {}", clean(error));
    }
    if record.state.active() {
        eprintln!(
            "Job {id} is still {} under its original lease until {}. \
             Inspect it with `proclease logs {id}` or end it with `proclease stop {id}`.",
            serde_json::to_value(record.state)?
                .as_str()
                .unwrap_or("active"),
            record.expires_at()
        );
    } else {
        eprintln!("Job {id} is not running. Inspect it with `proclease logs {id}`.");
    }
    Ok(1)
}

fn visible_jobs(store: &Store, all: bool) -> Result<(Vec<Record>, Vec<String>)> {
    let mut cleanup_warnings = match store.prune(false, None) {
        Ok(report) => report.warnings,
        Err(error) => vec![format!("history cleanup deferred: {error:#}")],
    };
    let (mut jobs, mut warnings) = store.list()?;
    warnings.append(&mut cleanup_warnings);
    select_visible(&mut jobs, all, now_ms());
    Ok((jobs, warnings))
}

fn select_visible(jobs: &mut Vec<Record>, all: bool, now: u64) {
    jobs.sort_by_key(|r| {
        let rank = if r.state.active() {
            0
        } else if r.state == State::Unknown || r.recent_history_at_ms().is_none() {
            1
        } else {
            2
        };
        (rank, r.created_at_ms)
    });
    if all {
        return;
    }
    let mut recent: Vec<_> = jobs
        .iter()
        .filter(|r| !r.state.active() && r.state != State::Unknown)
        .filter_map(|r| {
            r.recent_history_at_ms()
                .filter(|&time| time <= now && now - time < 30_000)
                .map(|time| (time, r.id.clone()))
        })
        .collect();
    recent.sort_by(|a, b| b.cmp(a));
    recent.truncate(5);
    jobs.retain(|r| {
        r.state.active()
            || r.state == State::Unknown
            || r.recent_history_at_ms().is_none()
            || recent.iter().any(|(_, id)| id == &r.id)
    });
}

type ContextEnv = [(OsString, OsString)];

/// Prefix of the current fingerprint encoding. Records written before it have
/// a bare 64-digit hash; see `legacy_fingerprint`.
const FINGERPRINT_V2: &str = "v2:";

/// Sorted environment entries that define a launch context. Only a digest of
/// these values is persisted, never the values themselves.
fn context_env() -> Vec<(OsString, OsString)> {
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| key != "_" && key != "SHLVL")
        .collect();
    env.sort();
    env
}

/// Domain-separated encoding with typed sections, element counts and byte
/// lengths, so moving bytes between argv, cwd and environment changes the hash.
fn fingerprint(argv: &[String], cwd: &Path, env: &ContextEnv) -> String {
    fn field(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    let mut digest = Sha256::new();
    digest.update(b"proclease launch context v2\0");
    digest.update(b"cwd\0");
    field(&mut digest, cwd.as_os_str().as_bytes());
    digest.update(b"argv");
    digest.update((argv.len() as u64).to_be_bytes());
    for arg in argv {
        field(&mut digest, arg.as_bytes());
    }
    digest.update(b"env\0");
    digest.update((env.len() as u64).to_be_bytes());
    for (key, value) in env {
        field(&mut digest, key.as_bytes());
        field(&mut digest, value.as_bytes());
    }
    format!("{FINGERPRINT_V2}{:x}", digest.finalize())
}

/// The pre-v2 encoding had no boundary between argv and environment, so argv
/// `[x, A, v]` with environment E aliased argv `[x]` with `A=v` plus E. It is
/// only used to verify records written by earlier versions.
fn legacy_fingerprint(argv: &[String], cwd: &Path, env: &ContextEnv) -> String {
    let mut digest = Sha256::new();
    digest.update(cwd.as_os_str().as_bytes());
    digest.update([0]);
    for arg in argv {
        digest.update(arg.as_bytes());
        digest.update([0]);
    }
    for (key, value) in env {
        digest.update(key.as_bytes());
        digest.update([0]);
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

/// Whether a retained launch has exactly this argv, directory and environment.
/// Argument vectors and directories are compared directly. A legacy hash is
/// accepted only after that comparison: with identical argv and cwd, the
/// remaining NUL-terminated key/value sequence identifies one environment
/// because OS arguments, paths and environment entries cannot contain NUL.
fn same_launch_context(previous: &Launch, argv: &[String], cwd: &Path, env: &ContextEnv) -> bool {
    if previous.argv != argv || previous.cwd != cwd {
        return false;
    }
    if previous.fingerprint.starts_with(FINGERPRINT_V2) {
        return previous.fingerprint == fingerprint(argv, cwd, env);
    }
    previous.fingerprint.len() == 64
        && previous
            .fingerprint
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        && previous.fingerprint == legacy_fingerprint(argv, cwd, env)
}

/// Launch identity requested by a keyed `run`, apart from its environment.
struct Requested<'a> {
    argv: &'a [String],
    cwd: &'a Path,
    owner: &'a str,
    task: Option<&'a str>,
    name: Option<&'a str>,
    url: Option<&'a str>,
    ttl_ms: u64,
    grace_ms: u64,
    log_limit_bytes: u64,
    mode: Mode,
}

/// Names of the non-environment launch fields in which a retained launch
/// differs from a request. Reports names only, never values. The mode is part
/// of the policy: a key never switches between detached and foreground jobs.
fn launch_differences(previous: &Launch, requested: &Requested) -> Vec<&'static str> {
    [
        ("mode", previous.mode != requested.mode),
        ("command", previous.argv != requested.argv),
        ("directory", previous.cwd != requested.cwd),
        ("owner", previous.owner != requested.owner),
        ("task", previous.task.as_deref() != requested.task),
        ("name", previous.name.as_deref() != requested.name),
        ("url", previous.url.as_deref() != requested.url),
        (
            "ttl",
            previous
                .expires_at_ms
                .saturating_sub(previous.created_at_ms)
                != requested.ttl_ms,
        ),
        ("grace", previous.grace_ms != requested.grace_ms),
        (
            "log limit",
            previous.log_limit_bytes != requested.log_limit_bytes,
        ),
    ]
    .into_iter()
    .filter_map(|(field, differs)| differs.then_some(field))
    .collect()
}

fn validate_label(value: &str) -> Result<()> {
    if value.len() > 512 || value.chars().any(char::is_control) {
        bail!("labels must be at most 512 bytes without control characters");
    }
    Ok(())
}

/// Bulk stop of a fixed selection. Exit status 1 when any selected job is not
/// confirmed finished (or, for a dry run, is unverified) or the scan warned.
fn stop_all(
    store: &Store,
    owner: Option<&str>,
    task: Option<&str>,
    dry_run: bool,
    json_output: bool,
    color: display::ColorChoice,
) -> Result<i32> {
    for label in [owner, task].into_iter().flatten() {
        validate_label(label)?;
    }
    // Dry runs take no retention protection; they consume nothing afterwards.
    let (selected, warnings, guard) = lifecycle::bulk_selection(store, owner, task, !dry_run)?;
    let results = if dry_run {
        selected
            .into_iter()
            .map(|record| {
                let unknown = record.state == State::Unknown;
                lifecycle::BulkResult {
                    selected: record.clone(),
                    record,
                    status: if unknown {
                        lifecycle::BulkStatus::Unverified
                    } else {
                        lifecycle::BulkStatus::WouldStop
                    },
                    error: unknown.then(|| {
                        "supervisor unavailable; it would not be signalled from disk".to_owned()
                    }),
                }
            })
            .collect()
    } else {
        let results = lifecycle::stop_selected(store, selected);
        // Every result is captured; release protection, then let normal
        // retention collect anything deferred while it was held.
        drop(guard);
        match store.prune(false, None) {
            Ok(report) => {
                for warning in report.warnings {
                    eprintln!("History cleanup: {}", clean(&warning));
                }
            }
            Err(error) => eprintln!("History cleanup deferred: {}", clean(&format!("{error:#}"))),
        }
        results
    };
    let complete = warnings.is_empty() && results.iter().all(|r| r.status.ok());
    if json_output {
        let entries: Vec<_> = results
            .iter()
            .map(|r| {
                json!({"id": r.selected.id, "selected_state": r.selected.state,
                "status": r.status.as_str(), "ok": r.status.ok(),
                "error": r.error, "job": view(&r.record)})
            })
            .collect();
        println!(
            "{}",
            json!({"schema_version": SCHEMA_VERSION, "dry_run": dry_run,
            "filters": {"owner": owner, "task": task},
            "complete": complete, "results": entries, "warnings": warnings})
        );
    } else {
        let records: Vec<_> = results.iter().map(|r| r.record.clone()).collect();
        if !records.is_empty() {
            display::print_table(&records, true, color, &resources::UsageMap::new())?;
        }
        let succeeded = results.iter().filter(|r| r.status.ok()).count();
        if dry_run {
            println!(
                "Would stop {succeeded} of {} selected jobs; nothing was changed.",
                results.len()
            );
        } else {
            println!("Stopped {succeeded} of {} selected jobs.", results.len());
        }
        for r in results.iter().filter(|r| !r.status.ok()) {
            eprintln!(
                "{}: {}: {}",
                r.selected.id,
                r.status.as_str(),
                clean(r.error.as_deref().unwrap_or("not confirmed"))
            );
        }
        for warning in &warnings {
            eprintln!("Warning: {}", clean(warning));
        }
    }
    Ok(if complete { 0 } else { 1 })
}

type SignalFlags = Vec<(i32, std::sync::Arc<std::sync::atomic::AtomicBool>)>;

/// Flags for the ordinary signals that end a foreground session. A repeated
/// signal exits immediately with 128 + its number.
fn foreground_signals() -> Result<SignalFlags> {
    [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
        .into_iter()
        .map(|signal| {
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            signal_hook::flag::register_conditional_shutdown(signal, 128 + signal, flag.clone())?;
            signal_hook::flag::register(signal, flag.clone())?;
            Ok((signal, flag))
        })
        .collect()
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGINT => "SIGINT",
        libc::SIGTERM => "SIGTERM",
        libc::SIGHUP => "SIGHUP",
        _ => "signal",
    }
}

/// Exit status for a foreground job after its keeper has finished: success
/// for a zero command exit, a requested stop or lease expiry; otherwise 1.
fn foreground_status(record: &Record) -> i32 {
    match record.state {
        State::Exited | State::Stopped | State::Expired => 0,
        _ => 1,
    }
}

/// The first signal set in `signals`, waiting for one indefinitely.
fn next_signal(signals: &SignalFlags) -> i32 {
    loop {
        if let Some(&(signal, _)) = signals
            .iter()
            .find(|(_, flag)| flag.load(std::sync::atomic::Ordering::Relaxed))
        {
            return signal;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Write best-effort notices on a detached thread. Stdout or stderr may be an
/// unread pipe or a stopped terminal; a blocked write then stalls only that
/// thread, never cleanup or exit. Write errors, and failure to start the
/// thread, are ignored.
fn notify(stdout: Option<String>, stderr: String) -> Option<std::thread::JoinHandle<()>> {
    use std::io::Write;
    std::thread::Builder::new()
        .spawn(move || {
            if let Some(line) = stdout {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
            }
            let _ = writeln!(std::io::stderr(), "{stderr}");
        })
        .ok()
}

/// Exit at once without flushing or locking stdout or stderr, so a blocked
/// report or notice cannot delay process exit.
fn exit_now(code: i32) -> ! {
    // SAFETY: `_exit` ends the process immediately; nothing is shared with it.
    unsafe { libc::_exit(code) }
}

/// Bounded wait for best-effort notices before exiting.
const NOTICE_WAIT: Duration = Duration::from_millis(250);

/// Handle the first ordinary signal for a foreground session: close the
/// keeper's liveness pipe, wait up to `bound` for its final record, and exit
/// with 128 + the signal number. Takes no report lock and performs no stdout
/// or stderr write itself, so neither an unread output pipe nor a report
/// blocked in another thread can delay cleanup or exit.
fn end_on_signal(
    signal: i32,
    session: &std::sync::Mutex<lifecycle::ForegroundSession>,
    bound: Duration,
    id: &str,
    startup_notice: bool,
) -> ! {
    use std::{sync::TryLockError, time::Instant};
    let deadline = Instant::now() + bound;
    let name = signal_name(signal);
    let mut notices: Vec<_> = notify(
        startup_notice.then(|| {
            json!({"schema_version": SCHEMA_VERSION, "job": {"id": id},
            "error": format!("{name} received before startup was reported; \
                              the foreground job is being stopped")})
            .to_string()
        }),
        format!("proclease: {name} received; stopping foreground job {id}"),
    )
    .into_iter()
    .collect();
    // The session lock is held elsewhere only for a non-blocking keeper poll
    // or a bounded end of its own, so the shared deadline still applies. It
    // stays held until exit, so the main thread cannot exit first with the
    // job's status while notices are pending.
    let mut held = loop {
        match session.try_lock() {
            Ok(guard) => break Some(guard),
            Err(TryLockError::Poisoned(error)) => break Some(error.into_inner()),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(TryLockError::WouldBlock) => break None,
        }
    };
    let confirmed = held
        .as_mut()
        .is_some_and(|session| session.end(deadline.saturating_duration_since(Instant::now())));
    if !confirmed {
        notices.extend(notify(
            None,
            format!(
                "proclease: cleanup of job {id} was not confirmed within {}; \
                 inspect `proclease list` before retrying",
                humantime::format_duration(bound)
            ),
        ));
    }
    let wait_until = Instant::now() + NOTICE_WAIT;
    while Instant::now() < wait_until && !notices.iter().all(|n| n.is_finished()) {
        std::thread::sleep(Duration::from_millis(10));
    }
    // `held` is never dropped: the session lock is released only by exit.
    exit_now(128 + signal)
}

/// Report startup (and readiness, if requested) as the single stdout object,
/// then hold this invocation until the keeper finishes the job. The first
/// ordinary SIGINT/SIGTERM/SIGHUP ends the session through the keeper with a
/// bounded wait that is independent of stdout/stderr and of the startup
/// report. Never returns an error, so no exit path skips that cleanup.
fn hold_foreground(
    store: &Store,
    session: lifecycle::ForegroundSession,
    signals: SignalFlags,
    record: Record,
    readiness: Option<&readiness::Spec>,
    json_output: bool,
    color: display::ColorChoice,
) -> i32 {
    use std::{
        io::Write,
        sync::{atomic::AtomicBool, atomic::Ordering, Arc, Mutex},
    };
    let id = record.id.clone();
    let bound = session.end_bound();
    let session = Arc::new(Mutex::new(session));
    // Claimed once, by the startup report or by a signal, so stdout carries
    // at most one object. Claiming never waits for a write in progress.
    let claimed = Arc::new(AtomicBool::new(false));
    {
        let (session, claimed, id) = (session.clone(), claimed.clone(), id.clone());
        // Runs until the process exits, so a signal also ends an invocation
        // whose final notice is blocked after the job finished.
        std::thread::spawn(move || {
            let signal = next_signal(&signals);
            let startup_notice = json_output && !claimed.swap(true, Ordering::SeqCst);
            end_on_signal(signal, &session, bound, &id, startup_notice)
        });
    }
    // Readiness is observed while signals stay handled, so a signal is
    // handled promptly during a long wait.
    let outcome = readiness.map(|spec| {
        (
            spec,
            wait_readiness(store, record.clone(), spec, json_output),
        )
    });
    let report = (|| -> Result<i32> {
        if claimed.swap(true, Ordering::SeqCst) {
            // A signal is ending the session and will exit.
            return Ok(0);
        }
        let code = match &outcome {
            Some((spec, outcome)) => report_readiness(outcome, false, spec, json_output, color)?,
            None => {
                print_record(&record, false, json_output, color)?;
                0
            }
        };
        std::io::stdout().flush()?;
        Ok(code)
    })();
    let mut code = report.unwrap_or_else(|error| {
        eprintln!("proclease: {}", clean(&format!("{error:#}")));
        1
    });
    if !json_output {
        eprintln!(
            "Holding foreground job {id} until it finishes; interrupting this command stops it."
        );
    }
    loop {
        let exited = session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keeper_exited();
        match exited {
            Ok(true) => break,
            Ok(false) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                // End first: the notice cannot delay cleanup if stderr blocks.
                session.lock().unwrap_or_else(|e| e.into_inner()).end(bound);
                eprintln!(
                    "proclease: cannot observe the foreground supervisor ({}); ended the session of job {id}",
                    clean(&format!("{error:#}"))
                );
                code = 1;
                break;
            }
        }
    }
    match store.observed_record(&id) {
        Ok(record) => {
            if !json_output {
                let state = serde_json::to_value(record.state)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                eprintln!("Foreground job {id} finished: {state}.");
                if let Some(error) = &record.error {
                    eprintln!("Note: {}", clean(error));
                }
            }
            code.max(foreground_status(&record))
        }
        Err(error) => {
            eprintln!(
                "proclease: final state of job {id} is unavailable: {}",
                clean(&format!("{error:#}"))
            );
            1
        }
    }
}

fn dispatch(cli: &Cli) -> Result<i32> {
    let store = Store::open(
        cli.state_dir
            .clone()
            .map(Ok)
            .unwrap_or_else(default_state)?,
    )?;
    match &cli.command {
        Commands::Keeper { id } => lifecycle::keeper(&store, id)?,
        Commands::Guard { id } => lifecycle::guard(&store, id)?,
        Commands::Run {
            ttl,
            owner,
            task,
            name,
            url,
            key,
            reuse,
            foreground,
            grace,
            wait_for_port,
            wait_for_log,
            wait_timeout,
            log_limit,
            command,
        } => {
            let mode = if *foreground {
                Mode::Foreground
            } else {
                Mode::Detached
            };
            let ttl = duration_ms(ttl)?;
            // Validated before the lock and before anything is launched.
            let readiness = readiness::spec(
                *wait_for_port,
                wait_for_log.as_deref(),
                wait_timeout.as_deref(),
            )?;
            let grace = duration_ms(grace)?;
            if grace > lifecycle::MAX_GRACE_MS {
                bail!("shutdown grace cannot exceed 10s");
            }
            if *log_limit == 0 || *log_limit > 16 * 1_048_576 {
                bail!("log limit must be between 1 byte and 16 MiB");
            }
            for label in [
                Some(owner),
                task.as_ref(),
                name.as_ref(),
                url.as_ref(),
                key.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                validate_label(label)?;
            }
            if command.iter().map(|s| s.len()).sum::<usize>() > 32_768 {
                bail!("command arguments exceed 32 KiB");
            }
            let cwd = std::env::current_dir()?.canonicalize()?;
            let env = context_env();
            let fingerprint = fingerprint(command, &cwd, &env);
            let mut signals = None;
            let (record, reused, session) = 'launch: {
                // Held through keeper startup: a failed keeper spawn is finalized
                // before another same-key run can observe the starting record.
                // Released before any readiness wait.
                let _lock = store.lock()?;
                let cleanup = store.prune_locked(false, None)?;
                for warning in cleanup.warnings {
                    eprintln!("History cleanup: {}", clean(&warning));
                }
                if let Some(key) = key {
                    if let Some((previous, record)) = store.find_retry(key)? {
                        let differences = launch_differences(
                            &previous,
                            &Requested {
                                argv: command,
                                cwd: &cwd,
                                owner,
                                task: task.as_deref(),
                                name: name.as_deref(),
                                url: url.as_deref(),
                                ttl_ms: ttl,
                                grace_ms: grace,
                                log_limit_bytes: *log_limit,
                                mode,
                            },
                        );
                        if !differences.is_empty() {
                            bail!(
                                "retry key already belongs to a different launch context \
                                 (differs: {}); nothing was launched",
                                differences.join(", ")
                            );
                        }
                        // --reuse deliberately keeps the retained job's original
                        // environment; an ordinary retry must match it exactly.
                        if !*reuse && !same_launch_context(&previous, command, &cwd, &env) {
                            bail!(
                                "retry key already belongs to a different launch context \
                                 (differs: environment); nothing was launched. Only if you \
                                 intend to return the retained job with its original launch \
                                 environment, retry explicitly with --reuse"
                            );
                        }
                        break 'launch (record, true, None);
                    }
                    if *reuse {
                        bail!(
                            "--reuse found no retained job for this retry key; \
                             nothing was launched"
                        );
                    }
                }
                // After retained-key lookup, so read-only reuse is unaffected.
                // Foreground jobs never create or need a Unix socket.
                if mode == Mode::Detached {
                    store.probe_control_socket()?;
                } else {
                    // Registered only when a foreground launch will happen and
                    // before anything starts, so an ordinary interrupt during
                    // startup reaches the bounded session cleanup. A second
                    // interrupt exits at once; the keeper then still sees its
                    // liveness pipe close and stops the job, unobserved.
                    signals = Some(foreground_signals()?);
                }
                let id = store.reserve()?;
                let created = now_ms();
                let launch = Launch {
                    id: id.clone(),
                    argv: command.clone(),
                    cwd,
                    owner: owner.clone(),
                    task: task.clone(),
                    name: name.clone(),
                    url: url.clone(),
                    key: key.clone(),
                    fingerprint,
                    created_at_ms: created,
                    expires_at_ms: created + ttl,
                    grace_ms: grace,
                    log_limit_bytes: *log_limit,
                    mode,
                    transport: mode.transport(),
                };
                debug_assert!(ttl <= MAX_LIFETIME_MS);
                atomic_json(&store.job(&id)?.join("launch.json"), &launch)?;
                store.save(&Record::new(&launch))?;
                let session = lifecycle::start_keeper(&store, &id, mode == Mode::Foreground)?;
                (store.record(&id)?, false, session)
            };
            match (session, signals) {
                (Some(session), Some(signals)) => {
                    return Ok(hold_foreground(
                        &store,
                        session,
                        signals,
                        record,
                        readiness.as_ref(),
                        cli.json,
                        cli.color,
                    ));
                }
                // A foreground launch that never started has nothing to stop;
                // a signal still ends this invocation even if its report blocks.
                (None, Some(signals)) => {
                    let _ = std::thread::Builder::new()
                        .spawn(move || exit_now(128 + next_signal(&signals)));
                }
                _ => {}
            }
            if reused && *foreground && !cli.json {
                eprintln!(
                    "The retained job was returned without attaching; this invocation does not \
                     hold its session."
                );
            }
            let Some(spec) = readiness else {
                print_record(&record, reused, cli.json, cli.color)?;
                return Ok(if record.state == State::Failed { 1 } else { 0 });
            };
            return print_readiness(&store, record, reused, &spec, cli.json, cli.color);
        }
        Commands::Prune { dry_run } => {
            let report = store.prune(*dry_run, None)?;
            if cli.json {
                println!(
                    "{}",
                    json!({"schema_version": SCHEMA_VERSION, "dry_run": dry_run,
                    "max_age_hours": 24, "max_finished": store::HISTORY_MAX_FINISHED,
                    "deferred": report.deferred, "candidates": report.candidates,
                    "removed": report.removed, "warnings": report.warnings})
                );
            } else {
                if *dry_run {
                    println!("Would remove {} finished jobs.", report.candidates.len());
                } else {
                    println!("Removed {} finished jobs.", report.removed.len());
                }
                for id in &report.candidates {
                    println!("{id}");
                }
                if report.deferred {
                    eprintln!(
                        "Job collection deferred while a bulk stop protects its selection; \
                         retention resumes after it finishes."
                    );
                }
                for warning in &report.warnings {
                    eprintln!("Warning: {}", clean(warning));
                }
            }
            if !report.warnings.is_empty() {
                return Ok(1);
            }
        }
        Commands::List { all, resources } => {
            let (jobs, warnings) = visible_jobs(&store, *all)?;
            let metrics = if !cli.json || *resources {
                resources::Sampler::snapshot(&jobs)
            } else {
                resources::UsageMap::new()
            };
            if cli.json {
                println!(
                    "{}",
                    json!({"schema_version": SCHEMA_VERSION, "jobs": jobs.iter().map(|r| job_view(r, &metrics, *resources)).collect::<Vec<_>>(), "warnings": warnings})
                );
            } else {
                display::print_table(&jobs, *all, cli.color, &metrics)?;
                for warning in warnings {
                    eprintln!("Warning: {}", clean(&warning));
                }
            }
        }
        Commands::Logs { id, tail } => {
            if *tail == 0 || *tail > 500 {
                bail!("tail must be between 1 and 500 lines");
            }
            let record = store.record(id)?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(store.job(id)?.join("output.log"))?;
            let mut bytes = Vec::new();
            file.take(16 * 1_048_576 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 16 * 1_048_576 {
                bail!("log exceeds supported bound");
            }
            let text = clean(&String::from_utf8_lossy(&bytes));
            let mut lines: Vec<_> = text.lines().rev().take(*tail).collect();
            lines.reverse();
            if cli.json {
                println!(
                    "{}",
                    json!({"schema_version": SCHEMA_VERSION, "id": id, "lines": lines, "logs_truncated": record.logs_truncated})
                );
            } else {
                for line in lines {
                    println!("{line}");
                }
                if record.logs_truncated {
                    eprintln!("Log capture reached its byte cap; newer output was discarded.");
                }
            }
        }
        Commands::Extend { id, by } => {
            let record = lifecycle::control(
                &store,
                id,
                Control::Extend {
                    by_ms: duration_ms(by)?,
                },
            )?;
            print_record(&record, false, cli.json, cli.color)?;
        }
        Commands::Stop { id: Some(id), .. } => {
            let record = lifecycle::control(&store, id, Control::Stop)?;
            print_record(&record, false, cli.json, cli.color)?;
        }
        Commands::Stop {
            id: None,
            all,
            owner,
            task,
            dry_run,
        } => {
            if !*all {
                bail!("specify a job ID or --all");
            }
            return stop_all(
                &store,
                owner.as_deref(),
                task.as_deref(),
                *dry_run,
                cli.json,
                cli.color,
            );
        }
        Commands::Watch {
            all,
            resources,
            interval,
            once,
        } => {
            let interval = duration_ms(interval)?;
            if interval < 200 {
                bail!("watch interval must be at least 200ms");
            }
            if cli.json && !once {
                bail!("use list --json or watch --once --json for bounded machine output");
            }
            let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            signal_hook::flag::register(libc::SIGINT, interrupted.clone())?;
            let mut sampler = resources::Sampler::new();
            loop {
                let (jobs, warnings) = visible_jobs(&store, *all)?;
                let metrics = if !cli.json || *resources {
                    if *once {
                        resources::Sampler::snapshot(&jobs)
                    } else {
                        sampler.sample(&jobs)
                    }
                } else {
                    resources::UsageMap::new()
                };
                if cli.json {
                    println!(
                        "{}",
                        json!({"schema_version": SCHEMA_VERSION, "jobs": jobs.iter().map(|r| job_view(r, &metrics, *resources)).collect::<Vec<_>>(), "warnings": warnings})
                    );
                } else {
                    if !once && std::io::stdout().is_terminal() {
                        print!("\x1b[2J\x1b[H");
                    }
                    display::print_table(&jobs, *all, cli.color, &metrics)?;
                    for warning in warnings {
                        eprintln!("Warning: {}", clean(&warning));
                    }
                }
                if *once || interrupted.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let next = std::time::Instant::now() + Duration::from_millis(interval);
                while std::time::Instant::now() < next
                    && !interrupted.load(std::sync::atomic::Ordering::Relaxed)
                {
                    std::thread::sleep(
                        Duration::from_millis(100)
                            .min(next.saturating_duration_since(std::time::Instant::now())),
                    );
                }
            }
        }
    }
    Ok(0)
}

fn main() {
    let cli = Cli::parse();
    match dispatch(&cli) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if cli.json {
                println!(
                    "{}",
                    json!({"schema_version": SCHEMA_VERSION, "error": format!("{error:#}")})
                );
            } else {
                eprintln!("proclease: {}", clean(&format!("{error:#}")));
            }
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        let mut env: Vec<_> = pairs
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect();
        env.sort();
        env
    }

    fn launch(argv: &[String], cwd: &Path, fingerprint: String) -> Launch {
        Launch {
            id: "000000000001".into(),
            argv: argv.to_vec(),
            cwd: cwd.to_path_buf(),
            owner: "user".into(),
            task: None,
            name: None,
            url: None,
            key: Some("k".into()),
            fingerprint,
            created_at_ms: 0,
            expires_at_ms: 1,
            grace_ms: 0,
            log_limit_bytes: 1,
            mode: Mode::Detached,
            transport: model::Transport::Socket,
        }
    }

    #[test]
    fn interrupted_history_shares_the_recent_window_and_cap() {
        let template = Record::new(&launch(&strings(&["true"]), Path::new("/work"), "f".into()));
        let now = 100_000;
        let mut jobs = Vec::new();
        for age in 1..=8 {
            let mut record = template.clone();
            record.id = format!("{age:012x}");
            record.state = State::Interrupted;
            if age % 2 == 0 {
                record.finished_at_ms = Some(now - age);
            } else {
                record.last_lifecycle_write_ms = Some(now - age);
            }
            jobs.push(record);
        }
        let mut stale = jobs[0].clone();
        stale.id = "stale".into();
        stale.last_lifecycle_write_ms = Some(now - 30_000);
        jobs.push(stale);
        for (id, state) in [("unknown", State::Unknown), ("active", State::Running)] {
            let mut record = jobs[0].clone();
            record.id = id.into();
            record.state = state;
            jobs.push(record);
        }
        let mut unverified = template;
        unverified.id = "unverified".into();
        unverified.state = State::Interrupted;
        jobs.push(unverified);
        let mut all = jobs.clone();
        select_visible(&mut all, true, now);
        assert_eq!(all.len(), jobs.len());
        select_visible(&mut jobs, false, now);
        let mut ids: Vec<_> = jobs.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            [
                "000000000001",
                "000000000002",
                "000000000003",
                "000000000004",
                "000000000005",
                "active",
                "unknown",
                "unverified"
            ]
        );
        let derived = jobs.iter().find(|r| r.id == "000000000001").unwrap();
        let json = serde_json::to_value(derived).unwrap();
        assert!(json["finished_at_ms"].is_null());
        assert!(json.get("last_lifecycle_write_ms").is_none());
        let restored: Record = serde_json::from_value(json).unwrap();
        assert!(restored.last_lifecycle_write_ms.is_none());
    }

    #[test]
    fn launch_differences_name_each_policy_field_without_values() {
        let argv = strings(&["/usr/bin/true", "secret-arg"]);
        let previous = launch(&argv, Path::new("/work"), "f".into());
        let same = Requested {
            argv: &argv,
            cwd: Path::new("/work"),
            owner: "user",
            task: None,
            name: None,
            url: None,
            ttl_ms: 1,
            grace_ms: 0,
            log_limit_bytes: 1,
            mode: Mode::Detached,
        };
        assert!(launch_differences(&previous, &same).is_empty());
        let other_argv = strings(&["/usr/bin/true"]);
        let cases: [(&str, Requested); 10] = [
            (
                "mode",
                Requested {
                    mode: Mode::Foreground,
                    ..same
                },
            ),
            (
                "command",
                Requested {
                    argv: &other_argv,
                    ..same
                },
            ),
            (
                "directory",
                Requested {
                    cwd: Path::new("/other"),
                    ..same
                },
            ),
            ("owner", Requested { owner: "x", ..same }),
            (
                "task",
                Requested {
                    task: Some("t"),
                    ..same
                },
            ),
            (
                "name",
                Requested {
                    name: Some("n"),
                    ..same
                },
            ),
            (
                "url",
                Requested {
                    url: Some("u"),
                    ..same
                },
            ),
            ("ttl", Requested { ttl_ms: 2, ..same }),
            (
                "grace",
                Requested {
                    grace_ms: 1,
                    ..same
                },
            ),
            (
                "log limit",
                Requested {
                    log_limit_bytes: 2,
                    ..same
                },
            ),
        ];
        for (field, requested) in cases {
            assert_eq!(launch_differences(&previous, &requested), [field]);
        }
    }

    #[test]
    fn fingerprint_sections_do_not_alias_and_legacy_records_are_verified() {
        let cwd = Path::new("/work");
        let path_only = env(&[("PATH", "/usr/bin:/bin")]);
        let with_a = env(&[("A", "value"), ("PATH", "/usr/bin:/bin")]);
        let long = strings(&["/usr/bin/true", "A", "value"]);
        let short = strings(&["/usr/bin/true"]);
        // The documented collision in the earlier encoding.
        assert_eq!(
            legacy_fingerprint(&long, cwd, &path_only),
            legacy_fingerprint(&short, cwd, &with_a)
        );
        assert_ne!(
            fingerprint(&long, cwd, &path_only),
            fingerprint(&short, cwd, &with_a)
        );
        assert_ne!(
            fingerprint(&strings(&["ab", "c"]), cwd, &path_only),
            fingerprint(&strings(&["a", "bc"]), cwd, &path_only)
        );

        let current = launch(&long, cwd, fingerprint(&long, cwd, &path_only));
        assert!(same_launch_context(&current, &long, cwd, &path_only));
        assert!(!same_launch_context(&current, &short, cwd, &with_a));
        assert!(!same_launch_context(&current, &long, cwd, &with_a));
        assert!(!same_launch_context(
            &current,
            &long,
            Path::new("/other"),
            &path_only
        ));

        // A legacy record is accepted for its own exact context only, never
        // for the colliding context that produces the same legacy hash.
        let legacy = launch(&long, cwd, legacy_fingerprint(&long, cwd, &path_only));
        assert!(same_launch_context(&legacy, &long, cwd, &path_only));
        assert!(!same_launch_context(&legacy, &short, cwd, &with_a));
        assert!(!same_launch_context(&legacy, &long, cwd, &with_a));
        let mut unknown = legacy.clone();
        unknown.fingerprint = format!("v9:{}", legacy.fingerprint);
        assert!(!same_launch_context(&unknown, &long, cwd, &path_only));
        unknown.fingerprint = legacy.fingerprint.to_uppercase();
        assert!(!same_launch_context(&unknown, &long, cwd, &path_only));
    }
}
