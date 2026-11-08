use std::env;

pub fn join_prefix_and_path(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    let path = path.trim_start_matches('/');

    if prefix.is_empty() || prefix == "/" {
        format!("/{}", path)
    } else if path.is_empty() {
        prefix.to_string()
    } else {
        format!("{}/{}", prefix, path)
    }
}

/// The repository this run was told it is with `--repo <owner/name>`
/// (carrick#2207), set once in `main` before anything reads a repository
/// name. `None` on every run that did not pass the flag, which then names
/// itself exactly as it always did.
///
/// A process-global, like the scan id, because its readers are spread across
/// the engine (the previous generation, the PR baseline, the own-copy and
/// sibling split in the join, the name `start-scan` is told), and threading
/// one value through each would change every caller for a flag only a runner
/// passes. See `docs/reference/runner.md`.
fn declared_slot() -> &'static std::sync::OnceLock<String> {
    static DECLARED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    &DECLARED
}

/// Record the `owner/name` this run was declared to be. One run is one
/// repository, so a second call is ignored rather than replacing it.
pub fn declare_repository(full_name: &str) {
    let _ = declared_slot().set(full_name.to_string());
}

/// The `owner/name` passed with `--repo`, or `None` when it was not.
pub fn declared_repository() -> Option<&'static str> {
    declared_slot().get().map(String::as_str)
}

/// The name of the indexed repo a local re-scan reads, set by the local check
/// on the scans it starts (carrick#3595). A re-scan of a linked git worktree
/// runs in the worktree's folder, whose name is not the repo's; this keeps its
/// blobs named as the repo the index holds. Local only: the runner never sets
/// it, `--repo` and `GITHUB_REPOSITORY` win over it, and the `owner/name` the
/// cloud is told never reads it.
pub const LOCAL_REPO_NAME_ENV: &str = "CARRICK_LOCAL_REPO_NAME";

/// The name a scan compares its own stored blobs against: which stored
/// generation it reuses, and which downloaded copies are this repo's rather
/// than siblings in the join.
///
/// `--repo` first, then `GITHUB_REPOSITORY`, each by its basename, then
/// [`LOCAL_REPO_NAME_ENV`], then the scanned directory's name.
pub fn get_repository_name(repo_path: &str) -> String {
    let github_repository = env::var("GITHUB_REPOSITORY").ok();
    let local = env::var(LOCAL_REPO_NAME_ENV)
        .ok()
        .filter(|name| !name.is_empty());
    let name = repository_name_from(
        repo_path,
        declared_repository(),
        github_repository.as_deref(),
        local.as_deref(),
    );

    // If we got "." (current directory), use the actual directory name
    if name == "."
        && let Ok(current_dir) = env::current_dir()
        && let Some(dir_name) = current_dir.file_name()
    {
        return dir_name.to_string_lossy().to_string();
    }

    name
}

/// [`get_repository_name`] with everything it reads passed in, so the
/// precedence is tested without the process environment: CI sets
/// `GITHUB_REPOSITORY` to the repo being built. A bare `.` is returned as it
/// is, for the caller to resolve against the working directory.
pub fn repository_name_from(
    repo_path: &str,
    declared: Option<&str>,
    github_repository: Option<&str>,
    local: Option<&str>,
) -> String {
    if let Some(full_name) = declared.or(github_repository)
        && let Some(repo_name) = full_name.split('/').next_back()
    {
        return repo_name.to_string();
    }
    if let Some(local) = local {
        return local.to_string();
    }

    repo_path
        .split('/')
        .rfind(|s| !s.is_empty())
        .unwrap_or(".")
        .to_string()
}

/// Reduce a source path to its repo-relative form: strip the repo-root prefix
/// when present, then any leading `./`. Mirrors the cache-key normalization in
/// `normalize_file_results_keys` so a full-scan absolute key
/// (`/abs/repo/src/api.ts`) and an incremental repo-relative key
/// (`src/api.ts`) reduce to the same string. A path outside the root passes
/// through unchanged.
///
/// Two callers depend on this being the SAME reduction the index stores as a
/// row's `file`: [`crate::type_manifest::build_site_id`], whose id is a join
/// key across the manifest and the infer request, and the file analyzer's
/// prompt, whose bytes the cloud's analysis cache hashes — an absolute path in
/// there makes every entry private to the directory that produced it
/// (carrick#1223).
pub fn repo_relative_source_path<'a>(file_path: &'a str, repo_root: &str) -> &'a str {
    let root = repo_root.trim_end_matches('/');
    let stripped = if root.is_empty() || root == "." {
        file_path
    } else {
        file_path
            .strip_prefix(root)
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(file_path)
    };
    stripped.strip_prefix("./").unwrap_or(stripped)
}

/// The UTF-16 code-unit offset that a UTF-8 byte offset into `content` names.
///
/// The type sidecar addresses nodes with ts-morph positions, and TypeScript
/// counts those in UTF-16 code units from zero. SWC counts bytes. The two
/// agree on every ASCII file and diverge, cumulatively, from a file's first
/// multi-byte character onwards — so a byte offset sent to the sidecar
/// unconverted resolves to the wrong node, or to none at all, everywhere
/// below that character (carrick#805).
///
/// A byte offset past the end of `content`, or one landing inside a
/// character, yields the offset of the last character it passed: this
/// converts a position, it does not validate one.
pub(crate) fn utf16_offset(content: &str, byte_offset: usize) -> u32 {
    let mut bytes = 0usize;
    let mut units = 0usize;
    for character in content.chars() {
        if bytes >= byte_offset {
            break;
        }
        bytes += character.len_utf8();
        units += character.len_utf16();
    }
    u32::try_from(units).unwrap_or(u32::MAX)
}

/// The ts-morph position of an SWC span into `content`: file-relative, and
/// counted in UTF-16 code units rather than bytes (carrick#805, carrick#806).
///
/// `span` is an SWC position: a UTF-8 byte offset counted from
/// [`crate::swc_scanner::SWC_SPAN_BASE`] into a file parsed on its own. Every
/// span the sidecar is sent passes through here: the receiver-type request,
/// the type layer's span locator (which the capture anchors are built from in
/// turn), and the key a site looks its compiler row up by (carrick#2405).
/// The conversion sits at the request and not at the candidate, because the
/// byte span is what joins a pass's answers back to its candidate.
///
/// `content` is the file as read from disk. A leading UTF-8 BOM is dropped
/// before counting: SWC's source map strips it, so an SWC span counts from
/// the character after it, and TypeScript drops it too (carrick#2407).
pub fn sidecar_position(content: &str, span: u32) -> u32 {
    utf16_offset(
        content.strip_prefix('\u{feff}').unwrap_or(content),
        span.saturating_sub(crate::swc_scanner::SWC_SPAN_BASE) as usize,
    )
}

/// [`sidecar_position`] over one file, for many spans (carrick#2564): each
/// conversion is a binary search over the file's characters wider in UTF-8
/// than in UTF-16, rather than a count from the start of the file, so a
/// file read at N positions costs N log L and not N x L.
#[derive(Debug)]
pub(crate) struct Utf16Offsets {
    /// The text's length in bytes, any BOM dropped.
    len: usize,
    /// Each character that is not ASCII, in order: its first byte, the byte
    /// after it, and how many more bytes than UTF-16 units the text holds up
    /// to and including it.
    wide: Vec<(usize, usize, usize)>,
}

impl Utf16Offsets {
    /// The table for `content`, the file as read from disk.
    pub(crate) fn new(content: &str) -> Self {
        let body = content.strip_prefix('\u{feff}').unwrap_or(content);
        let mut saved = 0;
        let wide = body
            .char_indices()
            .filter(|(_, character)| !character.is_ascii())
            .map(|(at, character)| {
                saved += character.len_utf8() - character.len_utf16();
                (at, at + character.len_utf8(), saved)
            })
            .collect();
        Self {
            len: body.len(),
            wide,
        }
    }

    /// What [`sidecar_position`] gives for `span` over the same text: every
    /// character that starts before the byte counts whole, and a byte past
    /// the end counts the whole text.
    pub(crate) fn position(&self, span: u32) -> u32 {
        let byte = (span.saturating_sub(crate::swc_scanner::SWC_SPAN_BASE) as usize).min(self.len);
        let before = self.wide.partition_point(|(start, _, _)| *start < byte);
        let (counted, saved) = match before.checked_sub(1).map(|last| self.wide[last]) {
            Some((_, end, saved)) => (byte.max(end), saved),
            None => (byte, 0),
        };
        u32::try_from(counted - saved).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table gives what `sidecar_position` gives at every byte of text
    /// mixing ASCII with two-, three- and four-byte characters, with and
    /// without a BOM, inside a character and past the end (carrick#2564).
    #[test]
    fn the_offset_table_converts_every_span_as_sidecar_position_does() {
        let base = crate::swc_scanner::SWC_SPAN_BASE;
        for content in [
            "",
            "plain ascii\r\n",
            "é",
            "😀",
            "a é b € c 😀 d\r\nconst s = \"日本語\";\n",
            "😀😀€éx",
            "\u{feff}",
            "\u{feff}const s = \"é\";\n",
            "\u{feff}plain",
        ] {
            let table = Utf16Offsets::new(content);
            for span in 0..=(content.len() as u32 + base + 3) {
                assert_eq!(
                    table.position(span),
                    sidecar_position(content, span),
                    "span {span} of {content:?}"
                );
            }
        }
    }

    /// A declared repository is the repository, whatever the environment and
    /// the folder say (carrick#2207); with nothing declared, the name is the
    /// one every run already used.
    #[test]
    fn a_declared_repository_wins_over_the_environment_and_the_folder() {
        assert_eq!(
            repository_name_from("/work/checkout", Some("acme/api"), Some("acme/other"), None),
            "api"
        );
        assert_eq!(
            repository_name_from("/work/checkout", Some("acme/api"), None, None),
            "api"
        );
        assert_eq!(
            repository_name_from("/work/checkout", None, Some("acme/other"), None),
            "other"
        );
        assert_eq!(
            repository_name_from("/work/checkout/", None, None, None),
            "checkout"
        );
        assert_eq!(repository_name_from(".", None, None, None), ".");
    }

    /// A local re-scan of a linked worktree is named as the indexed repo, and
    /// never over a name the runner declared (carrick#3595).
    #[test]
    fn a_local_repo_name_names_a_worktree_scan_below_the_declared_names() {
        assert_eq!(
            repository_name_from("/work/api/.claude/worktrees/b", None, None, Some("api")),
            "api"
        );
        assert_eq!(
            repository_name_from("/work/b", Some("acme/declared"), None, Some("api")),
            "declared"
        );
        assert_eq!(
            repository_name_from("/work/b", None, Some("acme/ci"), Some("api")),
            "ci"
        );
    }

    /// A path outside the repo root must pass through unchanged, never be
    /// mangled by a partial prefix match (`/repo` vs `/repo-other`).
    #[test]
    fn test_repo_relative_source_path_prefix_safety() {
        assert_eq!(
            repo_relative_source_path("/a/repo-other/src/x.ts", "/a/repo"),
            "/a/repo-other/src/x.ts"
        );
        assert_eq!(
            repo_relative_source_path("/a/repo/src/x.ts", "/a/repo"),
            "src/x.ts"
        );
        assert_eq!(repo_relative_source_path("src/x.ts", "/a/repo"), "src/x.ts");
        assert_eq!(repo_relative_source_path("./src/x.ts", "."), "src/x.ts");
        assert_eq!(
            repo_relative_source_path("/a/repo/src/x.ts", ""),
            "/a/repo/src/x.ts"
        );
    }

    #[test]
    fn an_ascii_file_converts_to_itself() {
        let content = "const a = 1;";
        assert_eq!(utf16_offset(content, 0), 0);
        assert_eq!(utf16_offset(content, 6), 6);
        assert_eq!(utf16_offset(content, content.len()), 12);
    }

    #[test]
    fn a_multi_byte_character_shifts_every_offset_below_it() {
        // `õ` is two UTF-8 bytes and one UTF-16 unit; the drift is one from
        // there on, and it accumulates with each further character.
        let content = "// plantões\nfetch('/a');";
        let call = content.find("fetch").expect("the call is in the source");
        assert_eq!(call, 13, "the call sits one byte past its UTF-16 index");
        assert_eq!(utf16_offset(content, call), 12);
    }

    #[test]
    fn a_character_outside_the_basic_plane_counts_as_two_units() {
        // An astral character is four UTF-8 bytes and a UTF-16 surrogate
        // pair, so it is the one case where the offset grows rather than
        // shrinks.
        let content = "// 🚀\nx";
        let x = content.find('x').expect("the binding is in the source");
        assert_eq!(utf16_offset(content, x), 6);
    }

    /// SWC's source map drops a leading BOM, so its spans count from the
    /// character after it, as TypeScript's positions do (carrick#2407). A
    /// span converted against the file as read lands where the same span
    /// lands in the file without the BOM; a file without one is unchanged.
    #[test]
    fn a_leading_bom_is_not_counted() {
        use swc_common::{FileName, SourceMap, sync::Lrc};
        let without = "const a = 1;\nfetch('/a');";
        let with = format!("\u{feff}{without}");
        let map: Lrc<SourceMap> = Default::default();
        let file = map.new_source_file(Lrc::new(FileName::Anon), with.clone());
        assert_eq!(&*file.src, without, "SWC parses the text without the BOM");
        let call = file.start_pos.0 + without.find("fetch").expect("the call") as u32;
        assert_eq!(file.start_pos.0, crate::swc_scanner::SWC_SPAN_BASE);

        assert_eq!(sidecar_position(&with, call), 13);
        assert_eq!(sidecar_position(without, call), 13);
        assert_eq!(sidecar_position(&with, file.start_pos.0), 0);
    }
}
