//! Where diagnostics go.
//!
//! While the TUI runs, the terminal belongs to ratatui. Anything written to
//! stderr lands on top of the rendered frame, and because ratatui only repaints
//! cells whose contents change, the damage stays on screen. The interactive UI
//! therefore logs to a file, while the headless commands log to stderr, where
//! nobody is drawing.

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

/// Environment variable that overrides the log file location.
pub const LOG_PATH_ENV: &str = "CRYPTUI_LOG";

/// Where log records should be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    /// Standard error, for commands that do not own the screen.
    Stderr,
    /// A file, for the interactive UI.
    File,
}

/// Start the global log subscriber.
///
/// Returns the file being written to, when one was opened — either because the
/// sink asked for it or because a requested file could not be created, in which
/// case records are discarded rather than sprayed over the UI.
pub fn init(verbosity: u8, sink: Sink) -> Option<PathBuf> {
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    match sink {
        Sink::Stderr => {
            tracing_subscriber::fmt()
                .with_env_filter(filter())
                .with_writer(std::io::stderr)
                .init();
            None
        }
        Sink::File => match log_path().and_then(open_log) {
            Some((path, file)) => {
                // No escape codes in a file, and one write per record.
                tracing_subscriber::fmt()
                    .with_env_filter(filter())
                    .with_ansi(false)
                    .with_writer(file)
                    .init();
                Some(path)
            }
            None => {
                tracing_subscriber::fmt()
                    .with_env_filter(filter())
                    .with_writer(std::io::sink)
                    .init();
                None
            }
        },
    }
}

/// Where the log file lives, if a location can be determined.
pub fn log_path() -> Option<PathBuf> {
    log_path_from(&|name| std::env::var(name).ok())
}

/// [`log_path`] with an explicit environment lookup, so it can be tested.
///
/// `$CRYPTUI_LOG` wins, then `$XDG_STATE_HOME/cryptui/cryptui.log`, then
/// `$HOME/.local/state/cryptui/cryptui.log`.
pub fn log_path_from(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(path) = env(LOG_PATH_ENV).filter(|path| !path.trim().is_empty()) {
        return Some(PathBuf::from(path));
    }

    let state = env("XDG_STATE_HOME")
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;

    Some(state.join("cryptui").join("cryptui.log"))
}

/// Create the log file and its directory, appending to what is there.
fn open_log(path: PathBuf) -> Option<(PathBuf, std::fs::File)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;

    Some((path, file))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{LOG_PATH_ENV, log_path_from};

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn the_override_wins() {
        let path = log_path_from(&env(&[
            (LOG_PATH_ENV, "/tmp/cryptui-test.log"),
            ("XDG_STATE_HOME", "/state"),
            ("HOME", "/home/tester"),
        ]));
        assert_eq!(path, Some(PathBuf::from("/tmp/cryptui-test.log")));
    }

    #[test]
    fn the_state_directory_is_used_when_set() {
        let path = log_path_from(&env(&[
            ("XDG_STATE_HOME", "/state"),
            ("HOME", "/home/tester"),
        ]));
        assert_eq!(
            path,
            Some(PathBuf::from("/state/cryptui/cryptui.log")),
            "the XDG state location is preferred over a guess from HOME"
        );
    }

    #[test]
    fn home_is_the_fallback() {
        let path = log_path_from(&env(&[("HOME", "/home/tester")]));
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/home/tester/.local/state/cryptui/cryptui.log"
            ))
        );
    }

    #[test]
    fn without_a_home_there_is_no_path() {
        assert_eq!(log_path_from(&env(&[])), None);
        assert_eq!(
            log_path_from(&env(&[(LOG_PATH_ENV, "   "), ("XDG_STATE_HOME", "")])),
            None,
            "blank values are ignored rather than treated as paths"
        );
    }
}
