//! Who started this `carrick` process, as the cloud's log reads it.
//!
//! The npm hooks (`npm/carrick/src/hook/*.ts`) run `carrick check` and
//! `carrick status` on a developer's machine and set `CARRICK_INVOKER` before
//! they spawn the binary. Every request this binary makes to the cloud carries
//! the value in `x-carrick-invoker`, so a hook fire shows up server-side
//! rather than being unmeasured. The header is the same on every protocol's
//! path: it names the caller, not an operation. Log-only on the cloud side; it
//! reaches no cache key and no response.

/// The request header.
pub const HEADER: &str = "x-carrick-invoker";

/// The environment variable the hooks set.
const ENV: &str = "CARRICK_INVOKER";

/// What no hook marked: a person at a shell, CI, or an older hook.
const DEFAULT: &str = "cli";

/// The values the cloud tells apart. Anything else reads as [`DEFAULT`], so a
/// stray variable in a user's shell never invents a value the log has to
/// explain.
const KNOWN: [&str; 5] = [
    "hook:post-edit",
    "hook:stop",
    "hook:session-start",
    "hook:post-read",
    DEFAULT,
];

/// The header value for this process.
pub fn current() -> &'static str {
    resolve(std::env::var(ENV).ok().as_deref())
}

fn resolve(raw: Option<&str>) -> &'static str {
    raw.and_then(|value| KNOWN.iter().copied().find(|known| *known == value.trim()))
        .unwrap_or(DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmarked_process_is_cli() {
        assert_eq!(resolve(None), "cli");
        assert_eq!(resolve(Some("")), "cli");
    }

    #[test]
    fn each_hook_value_is_sent_as_marked() {
        for hook in ["post-edit", "stop", "session-start", "post-read"] {
            let value = format!("hook:{hook}");
            assert_eq!(resolve(Some(&value)), value);
        }
        assert_eq!(resolve(Some(" hook:stop\n")), "hook:stop");
    }

    #[test]
    fn a_value_the_cloud_does_not_know_is_cli() {
        assert_eq!(resolve(Some("hook:other")), "cli");
        assert_eq!(resolve(Some("HOOK:STOP")), "cli");
    }

    #[test]
    fn the_header_name_is_a_legal_lowercase_header() {
        assert!(reqwest::header::HeaderName::from_bytes(HEADER.as_bytes()).is_ok());
        assert_eq!(HEADER, HEADER.to_ascii_lowercase());
    }
}
