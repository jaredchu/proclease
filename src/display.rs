use crate::{
    cell,
    model::{Record, State},
};
use anyhow::Result;
use clap::ValueEnum;
use std::{
    fmt::Write as _,
    io::{IsTerminal, Write as _},
    os::fd::AsRawFd,
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Default, ValueEnum)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

pub fn print_table(records: &[Record], all: bool, color: ColorChoice) -> Result<()> {
    let stdout = std::io::stdout();
    let colors = match color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            stdout.is_terminal()
                && std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").as_deref() != Ok("dumb")
        }
    };
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let detected = unsafe { libc::ioctl(stdout.as_raw_fd(), libc::TIOCGWINSZ, &mut size) };
    let width = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .or_else(|| (detected == 0 && size.ws_col > 0).then_some(size.ws_col as usize))
        .unwrap_or(100)
        .clamp(12, 160);
    stdout
        .lock()
        .write_all(render(records, all, colors, width, crate::model::now_ms()).as_bytes())?;
    Ok(())
}

fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn fit(text: &str, width: usize) -> String {
    let text = cell(text);
    if text.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let mut end = 0;
    for (offset, ch) in text.char_indices() {
        let next = offset + ch.len_utf8();
        if text[..next].width() + 1 > width {
            break;
        }
        end = next;
    }
    format!("{}…", &text[..end])
}

fn column(text: &str, width: usize) -> String {
    let text = fit(text, width);
    let padding = width.saturating_sub(text.width());
    format!("{text}{}", " ".repeat(padding))
}

fn state_label(state: State) -> (&'static str, &'static str) {
    match state {
        State::Running => ("● running", "32"),
        State::Starting => ("● starting", "33"),
        State::Stopping => ("● stopping", "33"),
        State::Unknown => ("? unknown", "1;33"),
        State::Failed => ("× failed", "31"),
        State::Interrupted => ("× interrupted", "33"),
        State::Stopped => ("○ stopped", "2"),
        State::Expired => ("○ expired", "2"),
        State::Exited => ("○ exited", "2"),
    }
}

fn left(record: &Record, now: u64) -> String {
    if record.state == State::Unknown {
        return "?".into();
    }
    if !record.state.active() {
        return "—".into();
    }
    let ms = record.expires_at_ms.saturating_sub(now);
    if ms == 0 {
        return "due".into();
    }
    let s = ms.div_ceil(1000);
    if s >= 86_400 {
        format!("{}d {}h", s / 86_400, s % 86_400 / 3600)
    } else if s >= 3600 {
        format!("{}h {:02}m", s / 3600, s % 3600 / 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

pub(crate) fn render(records: &[Record], all: bool, color: bool, width: usize, now: u64) -> String {
    let mut out = String::new();
    let active = records.iter().filter(|r| r.state.active()).count();
    let unknown = records.iter().filter(|r| r.state == State::Unknown).count();
    let finished = records.len() - active - unknown;
    let mut summary = format!("ProcLease  ·  {active} active");
    if unknown > 0 {
        let _ = write!(summary, "  ·  {unknown} unverified");
    }
    if all && finished > 0 {
        let _ = write!(summary, "  ·  {finished} finished");
    }
    let _ = writeln!(out, "\n{}", paint(&fit(&summary, width), "1;36", color));
    if records.is_empty() {
        let message = if all {
            "No managed jobs."
        } else {
            "No active jobs."
        };
        let _ = writeln!(out, "{}", fit(message, width));
        if !all {
            let _ = writeln!(
                out,
                "{}",
                paint(
                    &fit("Use --all to view finished history.", width),
                    "2",
                    color
                )
            );
        }
        return out;
    }
    let wide = width >= 72;
    let command_width = width.saturating_sub(49);
    if wide {
        let header = format!(
            "{}  {}  {}  {}  COMMAND",
            column("JOB", 12),
            column("STATE", 13),
            column("OWNER", 8),
            column("LEFT", 8)
        );
        let _ = writeln!(out, "{}", paint(&fit(&header, width), "1", color));
    }
    let _ = writeln!(out, "{}", paint(&"─".repeat(width), "2", color));
    for (index, record) in records.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let (state, tone) = state_label(record.state);
        let command = match &record.name {
            Some(name) => format!("{name} · {}", record.argv.join(" ")),
            None => record.argv.join(" "),
        };
        if wide {
            let _ = writeln!(
                out,
                "{}  {}  {}  {}  {}",
                paint(&column(&record.id, 12), "1", color),
                paint(&column(state, 13), tone, color),
                column(&record.owner, 8),
                column(&left(record, now), 8),
                fit(&command, command_width)
            );
        } else {
            let _ = writeln!(
                out,
                "{}",
                paint(&fit(&format!("{}  {state}", record.id), width), tone, color)
            );
            let _ = writeln!(out, "  {}", fit(&command, width.saturating_sub(2)));
            let timing = if record.state == State::Unknown {
                "unverified".to_owned()
            } else if record.state.active() {
                format!("{} left", left(record, now))
            } else {
                "finished".to_owned()
            };
            let _ = writeln!(
                out,
                "  {}",
                paint(
                    &fit(
                        &format!("{} · {timing}", record.owner),
                        width.saturating_sub(2)
                    ),
                    "2",
                    color
                )
            );
        }
        let name = record.cwd.file_name().unwrap_or_default().to_string_lossy();
        let project = if matches!(name.as_ref(), "site" | "app" | "web" | "src") {
            record
                .cwd
                .parent()
                .and_then(|p| p.file_name())
                .map(|p| format!("{}/{name}", p.to_string_lossy()))
                .unwrap_or_else(|| name.into_owned())
        } else {
            name.into_owned()
        };
        let mut detail = project;
        if let Some(task) = &record.task {
            let _ = write!(detail, " · {task}");
        }
        if record.state.active() {
            let expiry = record.expires_at();
            let _ = write!(
                detail,
                " · until {} {} UTC",
                &expiry[5..10],
                &expiry[11..16]
            );
        }
        let _ = writeln!(
            out,
            "  {}",
            paint(&fit(&detail, width.saturating_sub(2)), "2", color)
        );
        if let Some(url) = &record.url {
            let _ = writeln!(
                out,
                "  {}",
                paint(
                    &fit(&format!("↳ {url}"), width.saturating_sub(2)),
                    "36",
                    color
                )
            );
        }
        if let Some(error) = &record.error {
            let _ = writeln!(
                out,
                "  {}",
                paint(
                    &fit(&format!("! {error}"), width.saturating_sub(2)),
                    "33",
                    color
                )
            );
        }
        if record.logs_truncated {
            let _ = writeln!(
                out,
                "  {}",
                paint(
                    &fit(
                        "! Log capture capped; newer output was discarded.",
                        width.saturating_sub(2)
                    ),
                    "33",
                    color
                )
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Launch;

    #[test]
    fn unicode_and_untrusted_text_stay_inside_narrow_and_wide_views() {
        let launch = Launch {
            id: "0123456789ab".into(),
            argv: vec![
                "npm".into(),
                "run".into(),
                "預覽開發服务器 café 🚀\x1b[2J".repeat(8),
            ],
            cwd: "/workspace/網站/site".into(),
            owner: "codex".into(),
            task: Some("預覽任務 · café".repeat(8)),
            name: None,
            url: Some("http://127.0.0.1:4321/非常長的路徑".repeat(4)),
            key: None,
            fingerprint: String::new(),
            created_at_ms: 1_000,
            expires_at_ms: 6_001_000,
            grace_ms: 100,
            log_limit_bytes: 1024,
        };
        let mut record = Record::new(&launch);
        record.state = State::Running;
        for width in [20, 40, 80, 120] {
            let output = render(std::slice::from_ref(&record), false, false, width, 1_000);
            assert!(
                output.lines().all(|line| line.width() <= width),
                "overflow at {width}: {output}"
            );
            assert!(
                !output.contains('\x1b'),
                "application controls must be escaped"
            );
            assert!(output.contains(&record.id));
        }
        record.state = State::Unknown;
        let output = render(&[record], false, false, 80, 1_000);
        assert!(output.contains("unverified") && output.contains("? unknown"));
    }
}
