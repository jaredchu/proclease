#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("ProcLease targets macOS and Linux.");

mod display;
mod filectl;
mod foreground;
mod lifecycle;
mod model;
mod platform;
mod readiness;
mod resources;
mod run;
mod store;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use model::{duration_ms, now_ms, Control, Record, State, SCHEMA_VERSION};
use serde_json::json;
use std::{
    fs::OpenOptions,
    io::{IsTerminal, Read},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    time::Duration,
};
use store::Store;

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
    Run(run::RunArgs),
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

/// Unicode bidirectional formatting characters. They are not control
/// characters, but they visually reorder the surrounding text in a terminal,
/// so untrusted output could disguise what it shows ("Trojan Source",
/// CVE-2021-42574). Other format characters, such as the zero-width joiner in
/// emoji sequences, are left alone.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Untrusted text made safe for a terminal: control characters other than
/// newline and tab, and bidirectional formatting characters, are escaped.
fn clean(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| {
            if (c.is_control() && c != '\n' && c != '\t') || is_bidi_control(c) {
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
        Commands::Run(args) => return run::run(&store, args, cli.json, cli.color),
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
    use crate::model::{Launch, Mode};
    use std::path::Path;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
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
    fn terminal_text_escapes_controls_and_bidi_reordering_only() {
        // A right-to-left override would make this read differently on screen.
        let disguised = "safe\u{202E}txt.exe\u{2066}x\u{2069}\u{200F}\u{061C}\u{1b}[2J";
        let shown = clean(disguised);
        assert_eq!(
            shown,
            "safe\\u{202e}txt.exe\\u{2066}x\\u{2069}\\u{200f}\\u{61c}\\u{1b}[2J"
        );
        assert!(!shown.chars().any(|c| is_bidi_control(c) || c.is_control()));
        // Newlines and tabs stay for logs; cells flatten them to spaces.
        assert_eq!(clean("a\nb\tc"), "a\nb\tc");
        assert_eq!(cell("a\nb\tc"), "a b c");
        // Other format characters are kept: a zero-width joiner joins emoji.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} 預覽 café";
        assert_eq!(clean(family), family);
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
}
