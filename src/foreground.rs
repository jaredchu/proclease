//! Holding a `run --foreground` session until its job finishes, and ending
//! it on the first ordinary signal through the keeper.

use crate::{
    clean, display, lifecycle,
    model::{Record, State, SCHEMA_VERSION},
    print_record, readiness, run,
    store::Store,
};
use anyhow::Result;
use serde_json::json;
use std::time::Duration;

type SignalFlags = Vec<(i32, std::sync::Arc<std::sync::atomic::AtomicBool>)>;

/// Flags for the ordinary signals that end a foreground session. A repeated
/// signal exits immediately with 128 + its number.
pub fn signals() -> Result<SignalFlags> {
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
pub fn next_signal(signals: &SignalFlags) -> i32 {
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
pub fn exit_now(code: i32) -> ! {
    // SAFETY: `_exit` ends the process immediately; nothing is shared with it.
    unsafe { libc::_exit(code) }
}

/// Bounded wait for best-effort notices before exiting.
const NOTICE_WAIT: Duration = Duration::from_millis(250);

/// Handle the first ordinary signal for a foreground session: send the keeper
/// a stop request and close its liveness pipe, wait up to `bound` for its final
/// record, and exit with 128 + the signal number. Takes no report lock and
/// performs no stdout or stderr write itself, so neither an unread output pipe
/// nor a report blocked in another thread can delay cleanup or exit.
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
    // An ordinary signal is an orderly request to end the session, so the
    // job finishes as a requested stop.
    let confirmed = held.as_mut().is_some_and(|session| {
        session.request_stop(deadline.saturating_duration_since(Instant::now()))
    });
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
pub fn hold(
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
            run::wait_readiness(store, record.clone(), spec, json_output),
        )
    });
    let report = (|| -> Result<i32> {
        if claimed.swap(true, Ordering::SeqCst) {
            // A signal is ending the session and will exit.
            return Ok(0);
        }
        let code = match &outcome {
            Some((spec, outcome)) => {
                run::report_readiness(outcome, false, spec, json_output, color)?
            }
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
