//! `nun --lsp-log <path>`: the whole conversation, for a bug report.
//!
//! Every message each way, every line a server writes to stderr, and every
//! start, crash, restart and shutdown, stamped with the time since nun started.
//! Written on a thread of its own, so a slow disk slows the log and nothing
//! else.

use std::io::{self, Write as _};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::time::Instant;

/// Which way a line went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    /// From nun to the server.
    Sent,
    /// From the server to nun.
    Received,
    /// Something the server wrote to stderr.
    Stderr,
    /// Something nun did or noticed: a start, an exit, a timeout.
    Note,
}

impl Direction {
    const fn arrow(self) -> &'static str {
        match self {
            Self::Sent => "-->",
            Self::Received => "<--",
            Self::Stderr => "err",
            Self::Note => "---",
        }
    }
}

/// Where the conversation goes, if anywhere. Cheap to clone and to call when
/// there is no log: then it does nothing at all.
#[derive(Debug, Clone)]
pub(crate) struct Log {
    lines: Option<Sender<String>>,
    started: Instant,
}

impl Log {
    /// No log.
    pub(crate) fn none() -> Self {
        Self { lines: None, started: Instant::now() }
    }

    /// Log to the file at `path`, replacing whatever it held.
    ///
    /// # Errors
    ///
    /// If the file cannot be created.
    pub(crate) fn to_file(path: &Path) -> io::Result<Self> {
        let file = std::fs::File::create(path)?;
        let (lines, receiver) = mpsc::channel::<String>();
        std::thread::Builder::new().name("nun-lsp-log".into()).spawn(move || {
            let mut out = io::BufWriter::new(file);
            // Flushed whenever the queue runs dry rather than per line, so a
            // burst costs one write and a crash still loses almost nothing.
            while let Ok(line) = receiver.recv() {
                let _ = writeln!(out, "{line}");
                for line in receiver.try_iter() {
                    let _ = writeln!(out, "{line}");
                }
                let _ = out.flush();
            }
        })?;
        let log = Self { lines: Some(lines), started: Instant::now() };
        log.write(
            "nun",
            Direction::Note,
            &format!("nun {} language server log", env!("CARGO_PKG_VERSION")),
        );
        Ok(log)
    }

    /// Whether anything is being written.
    pub(crate) const fn is_on(&self) -> bool {
        self.lines.is_some()
    }

    /// Write one line about `server`.
    pub(crate) fn write(&self, server: &str, direction: Direction, text: &str) {
        let Some(lines) = &self.lines else { return };
        let elapsed = self.started.elapsed();
        let _ = lines.send(format!(
            "[{:>5}.{:03}] {server} {} {text}",
            elapsed.as_secs(),
            elapsed.subsec_millis(),
            direction.arrow()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_reach_the_file_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lsp.log");
        let log = Log::to_file(&path).unwrap();
        log.write("rust-analyzer", Direction::Sent, r#"{"id":1}"#);
        log.write("rust-analyzer", Direction::Received, r#"{"id":1,"result":null}"#);
        drop(log);
        // The writer is its own thread; give it a moment to catch up.
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let text = loop {
            let text = std::fs::read_to_string(&path).unwrap();
            if text.lines().count() >= 3 || Instant::now() > deadline {
                break text;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("language server log"), "{text}");
        assert!(lines[1].ends_with(r#"rust-analyzer --> {"id":1}"#), "{text}");
        assert!(lines[2].contains("<--"), "{text}");
    }

    #[test]
    fn no_log_costs_nothing_and_says_so() {
        let log = Log::none();
        assert!(!log.is_on());
        log.write("x", Direction::Note, "goes nowhere");
    }
}
