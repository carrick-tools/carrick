//! Every line this process writes for a person to read (carrick#1386).
//!
//! `println!` and `eprintln!` panic when the stream they write to answers with
//! an error, and the streams of a scan are taken away mid-run more often than
//! that shape allows for: an agent harness that stops a run closes the pty it
//! started it on and the next write returns `EIO`; a parent that held the
//! pipes and died leaves `EPIPE`. Either way the panic unwinds on the main
//! thread, out of the analysis future, past everything the end of a run does —
//! exit 101, no `scan-failed`, no log — and the handler that exists for
//! exactly that situation never gets to run.
//!
//! A run whose output nobody can read has nothing left to say to a terminal
//! and everything left to say to the cloud and to its own log file. So the
//! write is dropped and the run goes on: [`write_line`] takes the writer, so
//! the drop is one place rather than a `catch_unwind` over the whole run, and
//! [`outln`] and [`errln`] are what the rest of the scanner writes through.
//! The test in this module holds every other file to that, because the shape
//! that panics is the one every Rust program reaches for first.
//!
//! The other two writers a run has are safe already and stay as they are:
//! `tracing-subscriber` drops its own write errors (`log_internal_errors` is
//! off, pinned where the subscriber is built), and `indicatif` answers a
//! failed draw with an error it discards.

use std::io::Write;

/// Write one line to `out`, dropping the error a stream that has gone away
/// returns.
///
/// Takes the writer so that the drop can be proven by handing it one that
/// fails; the macros pass a locked handle, because a line written through two
/// unlocked writes is a line another thread can be interleaved into.
pub fn write_line(mut out: impl Write, line: std::fmt::Arguments<'_>) {
    let _ = writeln!(out, "{line}");
}

/// The same, for text that carries its own line breaks.
pub fn write_text(mut out: impl Write, text: std::fmt::Arguments<'_>) {
    let _ = write!(out, "{text}");
}

/// `println!`, for a stdout that may have gone away.
#[macro_export]
macro_rules! outln {
    () => {
        $crate::console::write_line(std::io::stdout().lock(), format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::console::write_line(std::io::stdout().lock(), format_args!($($arg)*))
    };
}

/// `print!`, for a stdout that may have gone away.
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::console::write_text(std::io::stdout().lock(), format_args!($($arg)*))
    };
}

/// `eprint!`, for a stderr that may have gone away.
#[macro_export]
macro_rules! err {
    ($($arg:tt)*) => {
        $crate::console::write_text(std::io::stderr().lock(), format_args!($($arg)*))
    };
}

/// `eprintln!`, for a stderr that may have gone away.
#[macro_export]
macro_rules! errln {
    () => {
        $crate::console::write_line(std::io::stderr().lock(), format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::console::write_line(std::io::stderr().lock(), format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};
    use std::path::{Path, PathBuf};

    /// A stream that has gone away: every write answers the way a closed pty
    /// and a dead parent's pipe do.
    struct Gone;

    impl Write for Gone {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(Error::new(ErrorKind::BrokenPipe, "Input/output error"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(Error::new(ErrorKind::BrokenPipe, "Input/output error"))
        }
    }

    /// The whole point: writing to a stream that is gone is not a panic.
    #[test]
    fn a_line_written_to_a_stream_that_is_gone_is_dropped() {
        write_line(Gone, format_args!("a report nobody can read"));
        write_line(Gone, format_args!(""));
    }

    /// And a line that can be written, is.
    #[test]
    fn a_line_written_to_a_stream_that_is_there_arrives() {
        let mut buffer: Vec<u8> = Vec::new();
        write_line(&mut buffer, format_args!("{} of {}", 3, 4));
        assert_eq!(String::from_utf8(buffer).expect("utf-8"), "3 of 4\n");
    }

    /// Every file under `src/` writes its user-facing lines through the macros
    /// above, because one `println!` on the path of a run is one panic between
    /// a stopped scan and the report it owes the cloud.
    ///
    /// Test modules are exempt and are where the remaining ones live: a test
    /// binary's stdout is the harness's, and a panic there is a failed test
    /// rather than an unreported scan. The text before a file's first
    /// `#[cfg(test)]` is what this reads.
    #[test]
    fn no_run_of_the_scanner_prints_through_a_macro_that_panics() {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders: Vec<String> = Vec::new();
        for file in rust_files(&src) {
            let text = std::fs::read_to_string(&file).expect("read a source file");
            let code = text.split("#[cfg(test)]").next().unwrap_or_default();
            for (number, line) in code.lines().enumerate() {
                let line = line.trim_start();
                if line.starts_with("//") || line.starts_with("///") {
                    continue;
                }
                for macro_name in ["println!", "eprintln!", "print!", "eprint!"] {
                    if line.contains(macro_name) {
                        offenders.push(format!(
                            "{}:{}: {macro_name}",
                            file.strip_prefix(&src).unwrap_or(&file).display(),
                            number + 1
                        ));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "write these through outln!/errln! so a closed terminal cannot panic a run \
             (carrick#1386):\n{}",
            offenders.join("\n")
        );
    }

    /// Every `.rs` file under `src/`, minus the sidecar's, which is
    /// TypeScript with a `node_modules` under it.
    fn rust_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "sidecar") {
                    continue;
                }
                files.extend(rust_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
        files
    }
}
