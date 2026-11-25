//! The `run` subcommand: launch identity, keyed retries, the launch itself
//! under the root lock, and readiness reporting. Foreground sessions are held
//! by `crate::foreground`.

use crate::{
    clean, display, foreground, lifecycle,
    model::{duration_ms, now_ms, Launch, Mode, Record, State, MAX_LIFETIME_MS, SCHEMA_VERSION},
    print_record, readiness,
    store::{atomic_json, Store},
    validate_label, view,
};
use anyhow::{bail, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{ffi::OsString, os::unix::ffi::OsStrExt, path::Path};

#[derive(clap::Args)]
pub struct RunArgs {
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
}

/// Launch a foreground command under an explicit lease.
pub fn run(
    store: &Store,
    args: &RunArgs,
    json_output: bool,
    color: display::ColorChoice,
) -> Result<i32> {
    let RunArgs {
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
    } = args;
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
            signals = Some(foreground::signals()?);
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
        let session = lifecycle::start_keeper(store, &id, mode == Mode::Foreground)?;
        (store.record(&id)?, false, session)
    };
    match (session, signals) {
        (Some(session), Some(signals)) => {
            return Ok(foreground::hold(
                store,
                session,
                signals,
                record,
                readiness.as_ref(),
                json_output,
                color,
            ));
        }
        // A foreground launch that never started has nothing to stop;
        // a signal still ends this invocation even if its report blocks.
        (None, Some(signals)) => {
            let _ = std::thread::Builder::new()
                .spawn(move || foreground::exit_now(128 + foreground::next_signal(&signals)));
        }
        _ => {}
    }
    if reused && *foreground && !json_output {
        eprintln!(
            "The retained job was returned without attaching; this invocation does not \
             hold its session."
        );
    }
    let Some(spec) = readiness else {
        print_record(&record, reused, json_output, color)?;
        return Ok(if record.state == State::Failed { 1 } else { 0 });
    };
    print_readiness(store, record, reused, &spec, json_output, color)
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

pub(crate) fn wait_readiness(
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

pub(crate) fn report_readiness(
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
            transport: crate::model::Transport::Socket,
        }
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
