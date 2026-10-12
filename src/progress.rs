//! How slow work reports what it is doing, and learns that it should stop.
//!
//! Each job (building an image, a download) takes a [`Reporter`]. The interactive interface's
//! reporter sends the events to its screen from the job's thread; the command line's prints them,
//! as text on stderr or as JSON lines on stdout (`--json`), which is what reims-ctl reads.

use serde::Serialize;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// A new phase of the job: "Downloading OpenCore 1.0.8".
    Step { message: String },
    /// Bytes done in the current phase, of `total` when it is known.
    Progress { done: u64, total: Option<u64> },
    /// A line worth keeping: a retry, a file written.
    Note { message: String },
}

pub trait Reporter: Sync {
    fn event(&self, e: Event);
    /// Whether the job should stop. A download stops between reads, keeping what it has, so
    /// running it again resumes.
    fn cancelled(&self) -> bool {
        false
    }

    fn step(&self, message: &str) {
        self.event(Event::Step {
            message: message.to_string(),
        });
    }
    fn progress(&self, done: u64, total: Option<u64>) {
        self.event(Event::Progress { done, total });
    }
    fn note(&self, message: &str) {
        self.event(Event::Note {
            message: message.to_string(),
        });
    }
}

/// The error a job returns when its reporter cancelled it.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

pub fn check(r: &dyn Reporter) -> anyhow::Result<()> {
    if r.cancelled() {
        return Err(Cancelled.into());
    }
    Ok(())
}

/// Prints events for a person at a terminal, on stderr: each step and note on a line of its own,
/// and progress on one line rewritten in place (at most ten times a second), or, when stderr is
/// not a terminal, a line every ten seconds.
pub struct TextReporter {
    tty: bool,
    state: Mutex<TextState>,
    pub cancel: AtomicBool,
}

struct TextState {
    last: Option<Instant>,
    on_progress_line: bool,
    rate: Rate,
}

impl TextReporter {
    pub fn new() -> Self {
        use std::io::IsTerminal;
        TextReporter {
            tty: std::io::stderr().is_terminal(),
            state: Mutex::new(TextState {
                last: None,
                on_progress_line: false,
                rate: Rate::default(),
            }),
            cancel: AtomicBool::new(false),
        }
    }
}

impl Reporter for TextReporter {
    fn event(&self, e: Event) {
        let Ok(mut s) = self.state.lock() else {
            return;
        };
        let mut err = std::io::stderr().lock();
        match e {
            Event::Step { message } | Event::Note { message } => {
                if s.on_progress_line {
                    let _ = writeln!(err);
                    s.on_progress_line = false;
                }
                let _ = writeln!(err, "{message}");
                s.last = None;
                s.rate = Rate::default();
            }
            Event::Progress { done, total } => {
                s.rate.add(done);
                let every = if self.tty {
                    Duration::from_millis(100)
                } else {
                    Duration::from_secs(10)
                };
                let finished = total == Some(done);
                if !finished && s.last.is_some_and(|t| t.elapsed() < every) {
                    return;
                }
                // A log's first line waits for something done.
                if !self.tty && done == 0 && !finished {
                    return;
                }
                s.last = Some(Instant::now());
                let line = progress_line(done, total, s.rate.per_second());
                if self.tty {
                    let _ = write!(err, "\r\x1b[2K  {line}");
                    s.on_progress_line = true;
                    if finished {
                        let _ = writeln!(err);
                        s.on_progress_line = false;
                    }
                } else {
                    let _ = writeln!(err, "  {line}");
                }
            }
        }
        let _ = err.flush();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Prints each event as a line of JSON on stdout, at most ten progress events a second.
pub struct JsonReporter {
    last: Mutex<Option<Instant>>,
}

impl JsonReporter {
    pub fn new() -> Self {
        JsonReporter {
            last: Mutex::new(None),
        }
    }
}

impl Reporter for JsonReporter {
    fn event(&self, e: Event) {
        if let Event::Progress { done, total } = e
            && total != Some(done)
            && let Ok(mut last) = self.last.lock()
        {
            if last.is_some_and(|t| t.elapsed() < Duration::from_millis(100)) {
                return;
            }
            *last = Some(Instant::now());
        }
        json_line(&e);
    }
}

pub fn json_line<T: Serialize>(v: &T) {
    if let Ok(s) = serde_json::to_string(v) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    }
}

/// "1.2 GiB of 15.6 GiB (8%), 24.0 MiB/s, 10 min left"
pub fn progress_line(done: u64, total: Option<u64>, per_second: Option<f64>) -> String {
    let mut s = match total {
        Some(t) if t > 0 => format!(
            "{} of {} ({}%)",
            bytes(done),
            bytes(t),
            (done as u128 * 100 / t as u128)
        ),
        _ => bytes(done),
    };
    if let Some(r) = per_second.filter(|r| *r > 0.0) {
        s.push_str(&format!(", {}/s", bytes(r as u64)));
        if let Some(t) = total
            && t > done
        {
            s.push_str(&format!(
                ", {} left",
                duration(((t - done) as f64 / r) as u64)
            ));
        }
    }
    s
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs.div_ceil(60)),
        _ => format!("{} h {} min", secs / 3600, (secs % 3600) / 60),
    }
}

/// A transfer rate over the last few seconds.
#[derive(Default)]
pub struct Rate {
    samples: std::collections::VecDeque<(Instant, u64)>,
}

impl Rate {
    pub fn add(&mut self, done: u64) {
        let now = Instant::now();
        if self.samples.back().is_some_and(|(_, d)| done < *d) {
            self.samples.clear();
        }
        self.samples.push_back((now, done));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(5))
        {
            self.samples.pop_front();
        }
    }

    pub fn per_second(&self) -> Option<f64> {
        let (t0, d0) = self.samples.front()?;
        let (t1, d1) = self.samples.back()?;
        let secs = t1.duration_since(*t0).as_secs_f64();
        if secs < 0.5 {
            return None;
        }
        Some((d1 - d0) as f64 / secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_and_times() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(15_664_523_626), "14.6 GiB");
        assert_eq!(duration(59), "59 s");
        assert_eq!(duration(61), "2 min");
        assert_eq!(duration(3700), "1 h 1 min");
        assert_eq!(
            progress_line(1 << 30, Some(4 << 30), Some(1024.0 * 1024.0)),
            "1.0 GiB of 4.0 GiB (25%), 1.0 MiB/s, 52 min left"
        );
    }

    #[test]
    fn events_serialize_as_tagged_json() {
        let e = Event::Progress {
            done: 5,
            total: Some(10),
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"event":"progress","done":5,"total":10}"#
        );
    }
}
