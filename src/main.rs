#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("ProcLease targets macOS and Linux.");

mod display;
mod lifecycle;
mod model;
mod platform;
mod resources;
mod store;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use model::{duration_ms, now_ms, Control, Launch, Record, State, MAX_LIFETIME_MS, SCHEMA_VERSION};
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
            help = "Explicit retry key; mismatched launch contexts are rejected"
        )]
        key: Option<String>,
        #[arg(long, default_value = "2s")]
        grace: String,
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
        } else if r.state == State::Unknown || r.finished_at_ms.is_none() {
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
            r.finished_at_ms
                .filter(|&time| time <= now && now - time < 30_000)
                .map(|time| (time, r.id.clone()))
        })
        .collect();
    recent.sort_by(|a, b| b.cmp(a));
    recent.truncate(5);
    jobs.retain(|r| {
        r.state.active()
            || r.state == State::Unknown
            || r.finished_at_ms.is_none()
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
    let (selected, warnings) = lifecycle::bulk_selection(store, owner, task)?;
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
        lifecycle::stop_selected(store, selected)
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
            grace,
            log_limit,
            command,
        } => {
            let ttl = duration_ms(ttl)?;
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
            // Held through keeper startup: a failed keeper spawn is finalized
            // before another same-key run can observe the starting record.
            let _lock = store.lock()?;
            let cleanup = store.prune_locked(false, None)?;
            for warning in cleanup.warnings {
                eprintln!("History cleanup: {}", clean(&warning));
            }
            if let Some(key) = key {
                if let Some((previous, record)) = store.find_retry(key)? {
                    if !same_launch_context(&previous, command, &cwd, &env)
                        || &previous.owner != owner
                        || &previous.task != task
                        || &previous.name != name
                        || &previous.url != url
                        || previous
                            .expires_at_ms
                            .saturating_sub(previous.created_at_ms)
                            != ttl
                        || previous.grace_ms != grace
                        || previous.log_limit_bytes != *log_limit
                    {
                        bail!("retry key already belongs to a different launch context");
                    }
                    print_record(&record, true, cli.json, cli.color)?;
                    return Ok(if record.state == State::Failed { 1 } else { 0 });
                }
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
            };
            debug_assert!(ttl <= MAX_LIFETIME_MS);
            atomic_json(&store.job(&id)?.join("launch.json"), &launch)?;
            store.save(&Record::new(&launch))?;
            lifecycle::start_keeper(&store, &id)?;
            let record = store.record(&id)?;
            print_record(&record, false, cli.json, cli.color)?;
            if record.state == State::Failed {
                return Ok(1);
            }
        }
        Commands::Prune { dry_run } => {
            let report = store.prune(*dry_run, None)?;
            if cli.json {
                println!(
                    "{}",
                    json!({"schema_version": SCHEMA_VERSION, "dry_run": dry_run,
                    "max_age_hours": 24, "max_finished": store::HISTORY_MAX_FINISHED,
                    "candidates": report.candidates, "removed": report.removed, "warnings": report.warnings})
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
