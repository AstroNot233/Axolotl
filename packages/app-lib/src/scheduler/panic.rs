//! Process-wide panic hook. Every panic, on any thread or task, is appended to
//! `panic.log` in the launcher log folder before the previous hook runs. The
//! record bypasses the tracing writer: a panic raised while that writer holds
//! its lock would otherwise deadlock inside the hook.

use std::io::Write;
use std::path::{Path, PathBuf};

pub const PANIC_LOG_FILE: &str = "panic.log";
/// The launcher's log pruning only removes `session_*` files, so the panic log
/// bounds itself: once it reaches this size it replaces `panic.old.log`.
pub const PANIC_LOG_MAX_BYTES: u64 = 1024 * 1024;
const PANIC_LOG_PREVIOUS_FILE: &str = "panic.old.log";

/// Installs the hook; call it once, as early in `main` as the log folder is known.
pub fn install_panic_hook(app_identifier: &str) {
    let log_dir =
        crate::state::DirectoryInfo::launcher_logs_dir_path(app_identifier);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = &log_dir {
            let thread = std::thread::current();
            let location = info
                .location()
                .map(|location| {
                    format!("{}:{}", location.file(), location.line())
                })
                .unwrap_or_default();
            let message = super::supervisor::panic_message(info.payload());
            let _ = append_panic_record(
                dir,
                PANIC_LOG_MAX_BYTES,
                thread.name().unwrap_or("unnamed"),
                &location,
                &message,
            );
        }
        previous(info);
    }));
}

/// Appends one line: RFC 3339 time, thread, location and message. A log that
/// already holds `max_bytes` is moved to `panic.old.log` first.
pub(crate) fn append_panic_record(
    dir: &Path,
    max_bytes: u64,
    thread: &str,
    location: &str,
    message: &str,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(PANIC_LOG_FILE);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= max_bytes) {
        std::fs::rename(&path, dir.join(PANIC_LOG_PREVIOUS_FILE))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let message = message.replace('\n', "\\n");
    writeln!(
        file,
        "{} panic thread={thread} at={location}: {message}",
        chrono::Local::now().to_rfc3339()
    )?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_records_are_appended_one_per_line() {
        let dir = tempfile::tempdir().unwrap();
        append_panic_record(dir.path(), 4096, "main", "src/a.rs:1", "first")
            .unwrap();
        let path = append_panic_record(
            dir.path(),
            4096,
            "worker",
            "src/b.rs:2",
            "second\nline",
        )
        .unwrap();
        let log = std::fs::read_to_string(path).unwrap();
        let lines: Vec<_> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("panic thread=main at=src/a.rs:1: first"));
        assert!(
            lines[1]
                .ends_with("panic thread=worker at=src/b.rs:2: second\\nline")
        );
    }

    #[test]
    fn a_full_log_is_rotated_once() {
        let dir = tempfile::tempdir().unwrap();
        let record = |message: &str| {
            append_panic_record(dir.path(), 64, "main", "src/a.rs:1", message)
                .unwrap()
        };
        record("first");
        record("second");
        let path = record("third");

        let current = std::fs::read_to_string(&path).unwrap();
        let previous =
            std::fs::read_to_string(dir.path().join(PANIC_LOG_PREVIOUS_FILE))
                .unwrap();
        assert!(current.ends_with(": third\n"));
        assert_eq!(current.lines().count(), 1);
        assert!(previous.contains(": second"));
        assert!(!previous.contains(": first"), "only one old log is kept");
    }
}
