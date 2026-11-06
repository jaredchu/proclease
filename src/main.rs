#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("ProcLease targets macOS and Linux.");

mod lifecycle;
mod model;
mod platform;
mod store;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use model::{duration_ms, now_ms, Control, Launch, Record, State, MAX_LIFETIME_MS, SCHEMA_VERSION};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{IsTerminal, Read, Write},
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::PathBuf,
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
    /// List managed leases; observing a lease never renews it.
    List,
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
    /// Stop one managed job and its supported process group.
    Stop { id: String },
    /// Show the live process table; Ctrl+C closes the view, not the jobs.
    Watch {
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

fn print_record(record: &Record, reused: bool, json_output: bool) -> Result<()> {
    if json_output {
        println!(
            "{}",
            json!({"schema_version": SCHEMA_VERSION, "job": view(record), "reused": reused})
        );
    } else {
        print_table(std::slice::from_ref(record))?;
        if reused {
            println!("Existing lease returned; its deadline was not extended.");
        }
        if let Some(error) = &record.error {
            eprintln!("{}", clean(error));
        }
    }
    Ok(())
}

fn print_table(records: &[Record]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{:<12} {:<11} {:<12} {:<10} {:<24} COMMAND",
        "ID", "STATE", "OWNER", "LEFT", "EXPIRES (UTC)"
    )?;
    for record in records {
        let state = serde_json::to_value(record.state)?
            .as_str()
            .unwrap()
            .to_owned();
        let left = if record.state.active() {
            humantime::format_duration(Duration::from_millis(
                record.expires_at_ms.saturating_sub(now_ms()),
            ))
            .to_string()
        } else {
            "-".to_owned()
        };
        writeln!(
            out,
            "{:<12} {:<11} {:<12} {:<10} {:<24} {}",
            record.id,
            state,
            cell(&record.owner),
            left,
            record.expires_at(),
            cell(&record.argv.join(" "))
        )?;
        if let Some(url) = &record.url {
            writeln!(out, "  Expected URL: {}", cell(url))?;
        }
        if record.logs_truncated {
            writeln!(
                out,
                "  Log capture capped at {} bytes; newer output is discarded.",
                record.log_limit_bytes
            )?;
        }
    }
    if records.is_empty() {
        writeln!(out, "No managed jobs.")?;
    }
    Ok(())
}

fn fingerprint(argv: &[String], cwd: &std::path::Path) -> String {
    let mut digest = Sha256::new();
    digest.update(cwd.as_os_str().as_bytes());
    digest.update([0]);
    for arg in argv {
        digest.update(arg.as_bytes());
        digest.update([0]);
    }
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| key != "_" && key != "SHLVL")
        .collect();
    env.sort();
    for (key, value) in env {
        digest.update(key.as_bytes());
        digest.update([0]);
        digest.update(value.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

fn validate_label(value: &str) -> Result<()> {
    if value.len() > 512 || value.chars().any(char::is_control) {
        bail!("labels must be at most 512 bytes without control characters");
    }
    Ok(())
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
            if grace > 10_000 {
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
            let fingerprint = fingerprint(command, &cwd);
            let _lock = store.lock()?;
            if let Some(key) = key {
                let (records, warnings) = store.list()?;
                if !warnings.is_empty() {
                    bail!("cannot safely resolve retry key: {}", warnings.join("; "));
                }
                for record in records {
                    let previous = store.launch(&record.id)?;
                    if previous.key.as_ref() == Some(key) {
                        if previous.fingerprint != fingerprint
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
                        print_record(&record, true, cli.json)?;
                        return Ok(if record.state == State::Failed { 1 } else { 0 });
                    }
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
            print_record(&record, false, cli.json)?;
            if record.state == State::Failed {
                return Ok(1);
            }
        }
        Commands::List => {
            let (jobs, warnings) = store.list()?;
            if cli.json {
                println!(
                    "{}",
                    json!({"schema_version": SCHEMA_VERSION, "jobs": jobs.iter().map(view).collect::<Vec<_>>(), "warnings": warnings})
                );
            } else {
                print_table(&jobs)?;
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
            print_record(&record, false, cli.json)?;
        }
        Commands::Stop { id } => {
            let record = lifecycle::control(&store, id, Control::Stop)?;
            print_record(&record, false, cli.json)?;
        }
        Commands::Watch { interval, once } => {
            let interval = duration_ms(interval)?;
            if interval < 200 {
                bail!("watch interval must be at least 200ms");
            }
            if cli.json && !once {
                bail!("use list --json or watch --once --json for bounded machine output");
            }
            let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            signal_hook::flag::register(libc::SIGINT, interrupted.clone())?;
            loop {
                let (jobs, warnings) = store.list()?;
                if cli.json {
                    println!(
                        "{}",
                        json!({"schema_version": SCHEMA_VERSION, "jobs": jobs.iter().map(view).collect::<Vec<_>>(), "warnings": warnings})
                    );
                } else {
                    if !once && std::io::stdout().is_terminal() {
                        print!("\x1b[2J\x1b[H");
                    }
                    print_table(&jobs)?;
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
