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

/// Get repository name, checking GITHUB_REPOSITORY environment variable first
pub fn get_repository_name(repo_path: &str) -> String {
    // Check for GitHub Actions environment variable (format: "owner/repo")
    if let Ok(github_repo) = env::var("GITHUB_REPOSITORY")
        && let Some(repo_name) = github_repo.split('/').next_back()
    {
        return repo_name.to_string();
    }

    // Fall back to extracting from path
    let path_name = repo_path
        .split("/")
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or(".");

    // If we got "." (current directory), use the actual directory name
    if path_name == "."
        && let Ok(current_dir) = env::current_dir()
        && let Some(dir_name) = current_dir.file_name()
    {
        return dir_name.to_string_lossy().to_string();
    }

    path_name.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
