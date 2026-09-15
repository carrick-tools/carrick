//! The last check a payload passes before it leaves the machine (carrick#1204).
//!
//! Every producer of payload text is meant to write it without machine paths
//! already: `relativize_cloud_paths` and [`super::served_paths::PathScrub`]
//! for served strings (carrick#1160), and the capture for its stub's
//! declaration files (carrick#1174). Each of those recognises the shapes it
//! was taught. This pass assumes one of them missed something.
//!
//! Just before upload it walks the serialized payload, every string and every
//! map key including each capture stub file, and replaces what still holds
//! the checkout root or the home directory with a neutral placeholder. It
//! never refuses the upload: the scan's own verdicts were computed before
//! this point against the tree on disk, and a machine path never resolved on
//! any other machine, so the placeholder costs nothing a reader of the index
//! could have used. It logs how many strings it rewrote per service, and at
//! debug level the field each one sat in (never its value).
//!
//! The in-memory payload is left untouched; the upload receives a scrubbed
//! copy, and only when there was something to scrub.

use crate::cloud_storage::CloudRepoData;
use tracing::{debug, warn};

/// Stands in for the checkout root.
pub(crate) const CHECKOUT_PLACEHOLDER: &str = "<checkout>";
/// Stands in for the home directory.
pub(crate) const HOME_PLACEHOLDER: &str = "<home>";

pub(crate) struct UploadBoundary {
    /// Machine prefixes to replace, longest first, each with its placeholder
    /// and every spelling it can appear in (as given, and with `/`).
    prefixes: Vec<(String, &'static str)>,
}

impl UploadBoundary {
    /// The boundary a scan applies: the checkout root it ran against and the
    /// home directory of the account running it.
    pub(crate) fn for_scan(repo_root: &str) -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok();
        // The analysis runs against the canonical root; the root as given
        // differs when the checkout is reached through a link.
        let canonical = std::fs::canonicalize(repo_root)
            .map(|path| path.to_string_lossy().to_string())
            .ok();
        let mut roots = vec![repo_root];
        roots.extend(canonical.as_deref());
        Self::with_roots(&roots, home.as_deref())
    }

    /// A boundary for an explicit root and home (tests build payloads under
    /// synthetic paths; a scan uses [`Self::for_scan`]).
    #[cfg(test)]
    pub(crate) fn new(repo_root: &str, home: Option<&str>) -> Self {
        Self::with_roots(&[repo_root], home)
    }

    fn with_roots(roots: &[&str], home: Option<&str>) -> Self {
        let mut prefixes: Vec<(String, &'static str)> = Vec::new();
        let mut add = |raw: &str, placeholder: &'static str| {
            let trimmed = raw.trim_end_matches(['/', '\\']);
            // A root of `/`, `.` or nothing names no machine, and a relative
            // root would rewrite repo-relative paths that are meant to stay.
            let absolute = trimmed.starts_with('/')
                || trimmed.starts_with("\\\\")
                || trimmed
                    .as_bytes()
                    .get(1..3)
                    .is_some_and(|s| s == b":\\" || s == b":/");
            if trimmed.is_empty() || !absolute {
                return;
            }
            for spelling in [trimmed.to_string(), trimmed.replace('\\', "/")] {
                if !prefixes.iter().any(|(known, _)| *known == spelling) {
                    prefixes.push((spelling, placeholder));
                }
            }
        };
        for root in roots {
            add(root, CHECKOUT_PLACEHOLDER);
        }
        if let Some(home) = home {
            add(home, HOME_PLACEHOLDER);
        }
        // The checkout usually sits under home: its longer prefix goes first.
        prefixes.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
        Self { prefixes }
    }

    /// `text` with every occurrence of a machine prefix that ends at a path
    /// boundary replaced, and how many occurrences there were. A prefix
    /// followed by more name characters (`/work/app` inside `/work/app2`) is
    /// another directory and stays.
    fn text(&self, text: &str) -> (String, usize) {
        let mut out = text.to_string();
        let mut hits = 0;
        for (prefix, placeholder) in &self.prefixes {
            if !out.contains(prefix.as_str()) {
                continue;
            }
            let mut rebuilt = String::with_capacity(out.len());
            let mut rest = out.as_str();
            while let Some(at) = rest.find(prefix.as_str()) {
                let after = &rest[at + prefix.len()..];
                let at_boundary = after
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.')));
                rebuilt.push_str(&rest[..at]);
                if at_boundary {
                    rebuilt.push_str(placeholder);
                    hits += 1;
                } else {
                    rebuilt.push_str(prefix);
                }
                rest = after;
            }
            rebuilt.push_str(rest);
            out = rebuilt;
        }
        (out, hits)
    }

    /// Replace machine prefixes throughout a JSON value; returns the strings
    /// rewritten, each with the dotted field path it sat in.
    fn value(&self, value: &mut serde_json::Value, field: &str, fields: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) => {
                let (scrubbed, hits) = self.text(s);
                if hits > 0 {
                    *s = scrubbed;
                    fields.push(field.to_string());
                }
            }
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    self.value(item, &format!("{field}[{index}]"), fields);
                }
            }
            serde_json::Value::Object(map) => {
                let entries = std::mem::take(map);
                for (key, mut item) in entries {
                    let (scrubbed_key, key_hits) = self.text(&key);
                    // A map key is data too (the file-results cache is keyed
                    // by path); name its field by the scrubbed key.
                    let child = if field.is_empty() {
                        scrubbed_key.clone()
                    } else {
                        format!("{field}.{scrubbed_key}")
                    };
                    if key_hits > 0 {
                        fields.push(format!("{child} (key)"));
                    }
                    self.value(&mut item, &child, fields);
                    map.insert(scrubbed_key, item);
                }
            }
            _ => {}
        }
    }

    /// The payload to upload: `None` when nothing in it holds a machine path
    /// (upload the original), otherwise a scrubbed copy. Logs the count for
    /// `service` and, at debug, each field.
    pub(crate) fn scrub(&self, data: &CloudRepoData, service: &str) -> Option<CloudRepoData> {
        if self.prefixes.is_empty() {
            return None;
        }
        let mut json = match serde_json::to_value(data) {
            Ok(json) => json,
            Err(error) => {
                warn!("Upload boundary could not read the payload of {service}: {error}");
                return None;
            }
        };
        let mut fields = Vec::new();
        self.value(&mut json, "", &mut fields);
        if fields.is_empty() {
            return None;
        }
        warn!(
            "Upload boundary replaced a machine path in {} string(s) of {service}; \
             an upstream pass missed them",
            fields.len()
        );
        for field in &fields {
            debug!("Upload boundary scrubbed {service}: {field}");
        }
        match serde_json::from_value(json) {
            Ok(scrubbed) => Some(scrubbed),
            Err(error) => {
                // The payload's own wire format failed to read back: nothing
                // sensible to upload in its place. Keep the upload (never
                // refuse it) and say so loudly.
                warn!(
                    "Upload boundary could not rebuild the scrubbed payload of {service} \
                     ({error}); uploading it as scanned"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary() -> UploadBoundary {
        UploadBoundary::new("/home/user/work/app", Some("/home/user"))
    }

    #[test]
    fn replaces_the_checkout_before_the_home_it_sits_in() {
        let (text, hits) = boundary()
            .text("import(\"/home/user/work/app/node_modules/x/index\").T | /home/user/.cache/y");
        assert_eq!(
            text,
            "import(\"<checkout>/node_modules/x/index\").T | <home>/.cache/y"
        );
        assert_eq!(hits, 2);
    }

    #[test]
    fn leaves_a_sibling_directory_that_only_shares_the_prefix() {
        let (text, hits) = boundary().text("/home/user/work/app2/src");
        assert_eq!(text, "<home>/work/app2/src");
        assert_eq!(hits, 1);
        let (text, hits) = boundary().text("/home/username/src");
        assert_eq!(text, "/home/username/src");
        assert_eq!(hits, 0);
    }

    #[test]
    fn replaces_a_bare_root_and_a_backslash_spelling() {
        let windows = UploadBoundary::new("C:\\Users\\user\\app", Some("C:\\Users\\user"));
        let (text, hits) = windows.text("C:\\Users\\user\\app and C:/Users/user/app/x");
        assert_eq!(text, "<checkout> and <checkout>/x");
        assert_eq!(hits, 2);
    }

    #[test]
    fn a_root_of_nothing_or_a_relative_root_names_no_machine() {
        assert!(UploadBoundary::new("", None).prefixes.is_empty());
        assert!(UploadBoundary::new(".", None).prefixes.is_empty());
        let relative = UploadBoundary::new("examples/app", None);
        let (text, hits) = relative.text("examples/app/src/a.ts");
        assert_eq!((text.as_str(), hits), ("examples/app/src/a.ts", 0));
        let (text, hits) = UploadBoundary::new("/", None).text("/usr/lib");
        assert_eq!((text.as_str(), hits), ("/usr/lib", 0));
    }
}
