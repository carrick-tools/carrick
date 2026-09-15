//! A ratchet against logging the things a run log must never carry
//! (carrick#1063).
//!
//! The laptop run log is uploaded now, so what the file layer writes is what
//! leaves a developer's machine. `redact_log` handles credentials and the home
//! directory; it cannot handle a `debug!` that formats a model response, a
//! prompt, or a file's source text, because those are not recognisable as
//! anything once they are in a line.
//!
//! So the rule is upstream of the redaction: at `debug!`, `info!` and `warn!`,
//! a value named like one of those things is not formatted. Raw model
//! responses are logged at `trace!` today (`file_analyzer_agent`,
//! `framework_detector`) and the file layer writes `debug` and above, so they
//! never reach the file — this is what keeps a later line from moving one up a
//! level without anyone noticing.
//!
//! Modelled on `.github/workflows/prompt-leak-guard.yml`, but as a test rather
//! than a workflow: this one has to read the syntax around a name (which macro,
//! positional or structured, and what the value is), and it belongs where
//! `cargo test` runs it.
//!
//! **There is no allow-list on purpose.** A hit is either a real leak, fixed by
//! logging a length instead of the value, or a name that means something else —
//! and the answer to the second is to rename the binding, not to except it.

use std::path::{Path, PathBuf};

/// Bare identifiers that name a value no log line may format.
///
/// Exact spellings, not a suffix rule: this codebase uses `source` for the
/// extractor that produced a row (`ResolutionSource`) and `type_import_source`
/// for a module specifier, and neither is a file's text. The list names the
/// spellings that ARE, and [`BANNED_SUFFIXES`] covers the compound forms that
/// can only mean one thing.
const BANNED_NAMES: [&str; 16] = [
    "response",
    "prompt",
    "body",
    "content",
    "contents",
    "source",
    "source_code",
    "source_text",
    "file_source",
    "file_content",
    "file_contents",
    "raw_source",
    "raw_content",
    "raw_body",
    "log_content",
    "body_text",
];

/// Compound spellings, whatever they are prefixed with: `model_response`,
/// `system_prompt`, `request_body`.
const BANNED_SUFFIXES: [&str; 3] = ["_response", "_prompt", "_body"];

/// Field accesses (`x.y`) are checked on their last field, against this
/// narrower set. `source` is deliberately absent: `entry.source` is a
/// provenance enum in this codebase and `held.source` is the same thing, so
/// including it would flag six honest lines and teach everyone to ignore the
/// test.
const BANNED_FIELDS: [&str; 5] = ["response", "prompt", "body", "content", "contents"];

/// The macros that reach the uploaded file. `trace!` does not: the file layer
/// filters at `debug`, which is exactly why the raw-response logging that
/// exists today is written at `trace!`.
const WATCHED_MACROS: [&str; 3] = ["debug", "info", "warn"];

/// A value reduced to a measurement is not the value. These are the
/// suffixes that say so.
fn is_a_measurement(value: &str) -> bool {
    [
        ".len()",
        ".count()",
        ".chars()",
        ".bytes()",
        ".lines()",
        ".is_empty()",
        ".is_some()",
        ".is_none()",
    ]
    .iter()
    .any(|call| value.contains(call))
}

fn banned_identifier(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    BANNED_NAMES.contains(&name.as_str())
        || BANNED_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix) && name.len() > suffix.len())
}

/// One flagged site.
#[derive(Debug)]
struct Hit {
    file: PathBuf,
    line: usize,
    argument: String,
}

/// Replace every string literal with an empty one, so the format string's own
/// words ("the response was short") are not read as identifiers.
fn without_string_literals(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '"' {
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == '\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] == '"' {
                    break;
                }
                j += 1;
            }
            out.push_str("\"\"");
            i = j + 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Split a macro's arguments on the commas that separate them, ignoring the
/// ones inside a nested call, index or block.
fn top_level_arguments(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for c in text.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if c == ',' && depth == 0 {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    parts.push(current);
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The text between a macro's parentheses, given the index of the opening one.
fn balanced_arguments(text: &str, open: usize) -> Option<&str> {
    let mut depth = 0i32;
    for (offset, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every `debug!`/`info!`/`warn!` invocation in `text`, as (line, arguments).
fn watched_invocations(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for macro_name in WATCHED_MACROS {
        let needle = format!("{macro_name}!(");
        let mut from = 0usize;
        while let Some(offset) = text[from..].find(&needle) {
            let at = from + offset;
            from = at + needle.len();
            // `let debug!` cannot happen, but `my_debug!(` can: the character
            // before the name must not continue an identifier or a path.
            let preceding = text[..at].chars().next_back();
            if preceding.is_some_and(|c| is_identifier_char(c) || c == '!') {
                continue;
            }
            if text[..at].ends_with("::") && !text[..at].ends_with("tracing::") {
                continue;
            }
            let open = at + needle.len() - 1;
            let Some(arguments) = balanced_arguments(text, open) else {
                continue;
            };
            found.push((text[..at].matches('\n').count() + 1, arguments.to_string()));
        }
    }
    found
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // The sidecar is TypeScript with its own node_modules; nothing
                // under it is a Rust logging site.
                if path.file_name().is_some_and(|n| n == "node_modules") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn scan(file: &Path, text: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (line, arguments) in watched_invocations(text) {
        for argument in top_level_arguments(&without_string_literals(&arguments)) {
            if is_a_measurement(&argument) {
                continue;
            }
            // A structured field: `name = value`, but not a comparison.
            if let Some((name, _)) = argument.split_once('=')
                && !argument.contains("==")
                && name
                    .trim()
                    .chars()
                    .all(|c| is_identifier_char(c) && c != '!')
                && !name.trim().is_empty()
            {
                if banned_identifier(name.trim()) {
                    hits.push(Hit {
                        file: file.to_path_buf(),
                        line,
                        argument: argument.clone(),
                    });
                }
                continue;
            }
            // A positional value, with any of the sigils a macro accepts.
            let value = argument.trim_start_matches(['&', '%', '?', '*']).trim();
            let is_path = !value.is_empty()
                && value.chars().all(|c| is_identifier_char(c) || c == '.')
                && !value.starts_with('.')
                && !value.ends_with('.')
                && value.chars().next().is_some_and(|c| !c.is_ascii_digit());
            if !is_path {
                continue;
            }
            let flagged = match value.rsplit_once('.') {
                None => banned_identifier(value),
                Some((_, last)) => BANNED_FIELDS.contains(&last.to_ascii_lowercase().as_str()),
            };
            if flagged {
                hits.push(Hit {
                    file: file.to_path_buf(),
                    line,
                    argument: argument.clone(),
                });
            }
        }
    }
    hits
}

/// No `debug!`, `info!` or `warn!` under `src/` formats a model response, a
/// prompt, or a file's source text.
///
/// Zero, not a baseline: today there are none, and the fix for a new one is to
/// log what it measures — a length, a count, an identifier — rather than to add
/// it here.
#[test]
fn no_logged_value_is_named_like_a_response_a_prompt_or_a_file() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for file in rust_sources(&src) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        hits.extend(scan(&file, &text));
    }

    assert!(
        hits.is_empty(),
        "these log sites format a value named like a model response, a prompt or a file's \
         source text, and the run log they write is uploaded (carrick#1063). Log what it \
         measures — a length, a count, an identifier — instead:\n{}",
        hits.iter()
            .map(|hit| format!("  {}:{}  {}", hit.file.display(), hit.line, hit.argument))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The guard catches what it is for. Without this, a scanner that silently
/// stopped matching anything would pass forever.
#[test]
fn the_guard_catches_the_shapes_it_is_written_for() {
    let sample = r#"
        fn f() {
            debug!("model said {}", response);
            info!(prompt = %composed, "sending");
            warn!("file {} said {}", path, file_contents);
            debug!("{}", resp.body);
            debug!("{}", model_response);
        }
    "#;
    let hits = scan(Path::new("sample.rs"), sample);
    assert_eq!(hits.len(), 5, "{hits:#?}");
}

/// And it does not catch what it is not for. A guard that flags honest lines
/// is a guard everyone learns to ignore.
#[test]
fn the_guard_leaves_honest_lines_alone() {
    let sample = r#"
        fn f() {
            debug!("model said {} bytes", response.len());
            trace!("raw {}", response);
            debug!("from {:?}", entry.source);
            debug!("import source {:?}", endpoint.type_import_source);
            info!(inferred_root_source = %root_source, "arbitration");
            debug!("the response was short");
            debug!(bytes = log_content.len(), "uploaded");
        }
    "#;
    assert!(scan(Path::new("sample.rs"), sample).is_empty());
}
