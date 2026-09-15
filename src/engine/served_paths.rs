//! Machine paths out of served type text (carrick#1160).
//!
//! The compiler prints a type whose declaring module the use site does not
//! import as `import("<absolute path>").Name`. On a scan that path is wherever
//! the module happened to live on the scanning machine: the checkout root, a
//! package manager's store under the account's home directory, a runtime's
//! package cache. Every one of those is served verbatim to every member of the
//! workspace through the index, so it leaks the scanning account's name and
//! directory layout, and it is most of the bytes of a typical signature.
//!
//! [`PathScrub::text`] rewrites the path TOKENS inside a string and leaves the
//! rest byte-identical:
//!
//! - a path into an installed package becomes `<name>@<version>` (or `<name>`
//!   when no version can be read), whatever layout put it there: a runtime's
//!   npm cache (`<cache>/npm/<registry-host>/<name>/<version>/...`), an
//!   isolated store (`node_modules/.pnpm/<name>@<version>/node_modules/<name>`,
//!   `node_modules/.deno/...` is the same shape), or a flat `node_modules`;
//! - a path under the checkout root becomes repo-relative;
//! - any other path under the home directory is reported from `~`.
//!
//! The package label is a label for a reader, not a module specifier: nothing
//! compiles these strings again. The capture stub's declaration tree IS
//! compiled at check time and is deliberately not passed through here.
//!
//! Structural throughout: the layouts are recognised by their directory
//! shape, never by a package, account or runner name.

use regex::Regex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

/// A run of path characters that contains at least one `/`. Delimiters are the
/// characters that bound a path in printed TypeScript (quotes, parentheses,
/// generics, separators) and in prose (whitespace). A parenthesised group with
/// no separator or space inside stays part of the path: an isolated store
/// writes peer dependencies into the entry name that way
/// (`.pnpm/<name>@<version>(<peer>@<version>)/`).
static PATH_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:[^\s"'`()<>\[\]{},;|&=]|\([^\s"'`()<>/]+\))*/(?:[^\s"'`()<>\[\]{},;|&=]|\([^\s"'`()<>/]+\))*"#,
    )
    .expect("valid regex")
});

/// An isolated store entry: `.pnpm/<name-with-+>@<version>[_peers|(peers)]/node_modules/<name>`.
/// `node_modules/.deno/` uses the same layout.
static ISOLATED_STORE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|/)\.(?:pnpm|deno)/([^/]+)/node_modules/((?:@[^/]+/)?[^/]+)")
        .expect("valid regex")
});

/// A runtime npm cache: `npm/<registry-host>/<name>/<version>/...`. The host
/// carries a dot and the version starts with a digit, which is what separates
/// this from an ordinary directory called `npm`.
static NPM_CACHE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|/)npm/[^/]+\.[^/]+/((?:@[^/]+/)?[^/]+)/(\d[^/]*)(?:/|$)")
        .expect("valid regex")
});

/// The last `node_modules/<name>` in a path (nested installs name the
/// innermost package).
static FLAT_NODE_MODULES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.*(?:^|/)node_modules/)((?:@[^/]+/)?[^/.][^/]*)").expect("valid regex")
});

pub(crate) struct PathScrub {
    /// Checkout root with a trailing `/`; empty when the root is unknown.
    root_prefix: String,
    /// Home directory with a trailing `/`, when one is known and is not `/`.
    home_prefix: Option<String>,
    /// Package directory -> version read from its `package.json`, memoised.
    versions: RefCell<HashMap<PathBuf, Option<String>>>,
}

impl PathScrub {
    /// The scrub a scan applies: the checkout root it ran against and the
    /// home directory of the account running it.
    pub(crate) fn for_scan(repo_root: &str) -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok();
        Self::new(repo_root, home.as_deref())
    }

    pub(crate) fn new(repo_root: &str, home: Option<&str>) -> Self {
        let prefix_of = |path: &str| {
            let trimmed = path.replace('\\', "/");
            let trimmed = trimmed.trim_end_matches('/');
            if trimmed.is_empty() || trimmed == "." {
                String::new()
            } else {
                format!("{trimmed}/")
            }
        };
        Self {
            root_prefix: prefix_of(repo_root),
            home_prefix: home.map(prefix_of).filter(|prefix| !prefix.is_empty()),
            versions: RefCell::new(HashMap::new()),
        }
    }

    /// `text` with every machine path token rewritten (see the module docs).
    /// Idempotent: a label, a repo-relative path and a `~/` path are all left
    /// as they are.
    pub(crate) fn text(&self, text: &str) -> String {
        if !text.contains('/') {
            return text.to_string();
        }
        PATH_TOKEN
            .replace_all(text, |caps: &regex::Captures| self.token(&caps[0]))
            .into_owned()
    }

    /// Rewrite `text` in place, only touching the allocation when it changes.
    pub(crate) fn in_place(&self, text: &mut String) {
        let rewritten = self.text(text);
        if rewritten != *text {
            *text = rewritten;
        }
    }

    fn token(&self, token: &str) -> String {
        let normalized = token.replace('\\', "/");
        if let Some(label) = self.package_label(&normalized) {
            return label;
        }
        if !self.root_prefix.is_empty()
            && let Some(at) = normalized.find(&self.root_prefix)
        {
            return normalized[at + self.root_prefix.len()..].to_string();
        }
        if let Some(home) = &self.home_prefix
            && let Some(at) = normalized.find(home.as_str())
        {
            return format!("~/{}", &normalized[at + home.len()..]);
        }
        token.to_string()
    }

    fn package_label(&self, path: &str) -> Option<String> {
        if let Some(caps) = ISOLATED_STORE.captures(path) {
            let entry = &caps[1];
            let name = &caps[2];
            let version = entry
                .strip_prefix(&format!("{}@", name.replace('/', "+")))
                .map(|rest| {
                    rest.split(['_', '('])
                        .next()
                        .unwrap_or_default()
                        .to_string()
                })
                .filter(|version| !version.is_empty());
            return Some(label(name, version));
        }
        if let Some(caps) = NPM_CACHE.captures(path) {
            return Some(label(&caps[1], Some(caps[2].to_string())));
        }
        if let Some(caps) = FLAT_NODE_MODULES.captures(path) {
            let name = &caps[2];
            let package_dir = PathBuf::from(format!("{}{}", &caps[1], name));
            return Some(label(name, self.installed_version(package_dir)));
        }
        None
    }

    /// The version an installed package declares. Only an absolute package
    /// directory is read; a relative one names no directory on this machine.
    fn installed_version(&self, package_dir: PathBuf) -> Option<String> {
        if !package_dir.is_absolute() {
            return None;
        }
        self.versions
            .borrow_mut()
            .entry(package_dir)
            .or_insert_with_key(|dir| {
                let manifest = std::fs::read_to_string(dir.join("package.json")).ok()?;
                let value: serde_json::Value = serde_json::from_str(&manifest).ok()?;
                value
                    .get("version")
                    .and_then(|version| version.as_str())
                    .map(str::to_string)
            })
            .clone()
    }
}

fn label(name: &str, version: Option<String>) -> String {
    match version {
        Some(version) => format!("{name}@{version}"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/home/user/work/acme";
    const HOME: &str = "/home/user";

    fn scrub() -> PathScrub {
        PathScrub::new(ROOT, Some(HOME))
    }

    #[test]
    fn runtime_npm_cache_paths_become_package_labels() {
        let text = "(c: import(\"/home/user/.cache/deno/npm/registry.npmjs.org/web-kit/4.12.12/dist/types/context\").Context<Env, \"/\", import(\"/home/user/.cache/deno/npm/registry.npmjs.org/web-kit/4.12.12/dist/types/types\").BlankInput>) => Promise<Response>";
        assert_eq!(
            scrub().text(text),
            "(c: import(\"web-kit@4.12.12\").Context<Env, \"/\", import(\"web-kit@4.12.12\").BlankInput>) => Promise<Response>"
        );
    }

    #[test]
    fn scoped_packages_keep_their_scope() {
        assert_eq!(
            scrub().text("import(\"/var/cache/npm/registry.example.org/@acme/schema/9.6.0/build/protos\").Doc"),
            "import(\"@acme/schema@9.6.0\").Doc"
        );
    }

    #[test]
    fn isolated_store_paths_read_name_and_version_from_the_entry() {
        let scrub = scrub();
        assert_eq!(
            scrub.text("import(\"/home/user/work/acme/node_modules/.pnpm/@acme+kit@2.0.1_react@18.2.0/node_modules/@acme/kit/dist/index\").Kit"),
            "import(\"@acme/kit@2.0.1\").Kit"
        );
        assert_eq!(
            scrub.text("import(\"/home/user/work/acme/node_modules/.deno/@types+react@19.2.14/node_modules/@types/react/index\").JSX.Element"),
            "import(\"@types/react@19.2.14\").JSX.Element"
        );
        assert_eq!(
            scrub.text("import(\"node_modules/.pnpm/left-pad@1.3.0(typescript@5.8.2)/node_modules/left-pad/index\").Pad"),
            "import(\"left-pad@1.3.0\").Pad"
        );
    }

    #[test]
    fn flat_node_modules_read_the_installed_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let package = dir.path().join("node_modules/@acme/flat");
        std::fs::create_dir_all(&package).expect("package dir");
        std::fs::write(package.join("package.json"), r#"{"version":"3.1.4"}"#).expect("manifest");
        let root = dir.path().to_string_lossy().to_string();
        let scrub = PathScrub::new(&root, None);
        assert_eq!(
            scrub.text(&format!(
                "import(\"{root}/node_modules/@acme/flat/lib/types\").Flat"
            )),
            "import(\"@acme/flat@3.1.4\").Flat"
        );
        // No manifest on disk: the name alone, never a guessed version.
        assert_eq!(
            scrub.text(&format!("import(\"{root}/node_modules/missing/index\").M")),
            "import(\"missing\").M"
        );
    }

    #[test]
    fn checkout_root_is_stripped_anywhere_in_a_token() {
        let scrub = scrub();
        assert_eq!(
            scrub.text("import(\"/home/user/work/acme/src/types\").Req"),
            "import(\"src/types\").Req"
        );
        assert_eq!(
            scrub.text("source file not in program: /home/user/work/acme/packages/utils/id.ts"),
            "source file not in program: packages/utils/id.ts"
        );
    }

    #[test]
    fn prose_paths_are_rewritten_inside_quotes_including_relative_escapes() {
        let scrub = scrub();
        assert_eq!(
            scrub.text("declaration emit was skipped for module '/home/user/.cache/deno/npm/registry.npmjs.org/web-kit/4.12.12/dist/types/base'; alias demoted"),
            "declaration emit was skipped for module 'web-kit@4.12.12'; alias demoted"
        );
        assert_eq!(
            scrub.text("module '../../../../../home/user/Library/Caches/deno/npm/registry.npmjs.org/result-kit/8.2.0/dist'"),
            "module 'result-kit@8.2.0'"
        );
    }

    #[test]
    fn other_paths_under_home_are_reported_from_tilde() {
        assert_eq!(
            scrub().text("import(\"/home/user/elsewhere/shared/types\").Shared"),
            "import(\"~/elsewhere/shared/types\").Shared"
        );
    }

    #[test]
    fn type_text_without_machine_paths_is_untouched_and_the_scrub_is_idempotent() {
        let scrub = scrub();
        for text in [
            "{ contentType: \"text/html\"; url: `/api/${string}`; }",
            "import(\"src/types\").Req",
            "import(\"web-kit@4.12.12\").Context",
            "Array<string> | null",
        ] {
            assert_eq!(scrub.text(text), text);
            assert_eq!(scrub.text(&scrub.text(text)), text);
        }
    }

    #[test]
    fn no_home_means_no_tilde_rewrite() {
        let scrub = PathScrub::new(ROOT, None);
        assert_eq!(
            scrub.text("import(\"/opt/shared/types\").Shared"),
            "import(\"/opt/shared/types\").Shared"
        );
    }
}
