# Running a scan from a runner

The scanner's side of the contract with a process that runs `carrick` on a
checkout it made itself: a container the cloud starts, or any CI other than
GitHub Actions. A GitHub Actions run needs none of this; the Action passes no
flags and the scan reads its context from `GITHUB_*`.

## The command

```
carrick <path> --repo <owner/name> --upload
```

- `--repo <owner/name>` is the repository the checkout is. It decides:
  - the name the scan compares its stored blobs against: which stored
    generation it reuses, so whether unchanged files replay their answers or
    go to the model again;
  - which downloaded copies are this repository's own and which are siblings
    in the cross-repo join;
  - the repository `start-scan` and the preflight-failed marker name.

  Without it the repository is the basename of `GITHUB_REPOSITORY`, else the
  checkout folder's name, and the full name sent to the cloud comes from the
  origin remote. When the flag and the origin remote disagree, the run logs it
  at info and uses the flag. The value must be `owner/name`; anything else is a
  usage error.
- `--upload` / `--no-upload` decides whether the scan uploads its index.
  Without either, the scan decides from the GitHub context: no upload on a
  `pull_request` event or a `refs/pull/*` ref, upload on `refs/heads/main` and
  `refs/heads/master` only, and upload when nothing is set.
  `CARRICK_OUTPUT_JSON` never uploads, and `--upload` with it is a usage error.
  A `--no-upload` run that opened a scan and finished ends it with
  `close-scan`, reason `no_upload`, so its slot is released straight away. A
  run that failed reports `scan-failed` instead.

- `--changed-only` analyses only the services whose inputs changed since
  their stored index, and carries the rest (carrick#2227,
  `src/change_scope.rs`). A carried service is not analysed and the type
  checker is never asked about it: its stored generation stands in for it
  in the cross-service join, as a held-back service's does. Together with
  `--no-cache` it is a usage error.

- `--pr <number> --pr-head <sha> --pr-base <sha>` scan a pull request's tree
  outside GitHub Actions (carrick#2229, `src/pr_context.rs`):

  ```
  carrick <path> --repo <owner/name> --pr <n> --pr-head <sha> --pr-base <sha>
  ```

  Optionally with `--changed-only`. All three or none; the number is a
  positive integer and each sha is 40 hex characters. The flags win over
  `GITHUB_REF`, `GITHUB_RUN_ID` and the `GITHUB_EVENT_PATH` payload, which
  otherwise supply the same four facts. The result the scan posts carries the
  number and head from the flags and no run id.

  A pull request scan never uploads: `--pr` with `--upload` is a usage error,
  and a run that opened a scan ends it with `close-scan`, reason
  `pull_request`, including when `--changed-only` analysed nothing.

  The checkout must be the pull request's tree, checked before anything is
  analysed:

  | HEAD | Checkout | Base |
  |---|---|---|
  | A merge commit whose second parent is `--pr-head` | merge | the first parent |
  | `--pr-head` itself | head (a PR with conflicts) | `--pr-base`, the merge base rather than the base branch's tip |
  | Anything else | refused, exit 2 | |

None of these flags is listed in `carrick --help`.

## What `--changed-only` analyses

A service's base is its own stored blob's commit; services of one
repository can sit at different commits. The paths changed since that
commit are listed from the repository's top level, against the working
tree, untracked files included.

What a service reads, computed on the tree being scanned:

- its roots: `directory` and every `include`, minus the directories of
  services nested inside it;
- its import closure: every walked file's imports and, transitively, the
  imports of every repository file they reach, resolved by the scanner's own
  resolver (relative paths, tsconfig `paths`, workspace package names);
- every workspace package that closure enters, as its whole directory;
- its tsconfig (the one `carrick.json` names, else the nearest
  `tsconfig.json` at or above its directory), every config its `extends`
  chain reads, and each directory or file that config's `include` or
  `files` names;
- its manifest and its declared `graphqlSchemas`.

A service is analysed, and the run logs why, when:

| Reason | Condition |
|---|---|
| `no_stored` | No stored blob for this repository and service. |
| `other_scanner` | The stored blob's cache version is not this run's, or (on a run that asks the model) its scanner version is not. |
| `stored_dirty` | The stored blob was read from uncommitted changes. |
| `stored_incomplete` | The stored blob records degraded types, or (on a run that asks the model) files the model never answered or candidates no model was asked about. |
| `config_changed` | Its `carrick.json` entry differs from the stored one. |
| `commit_missing` | git cannot compare the stored commit, for example because the clone lacks it. |
| `unresolved_import` | An import in it or in its closure resolves to nothing, is neither a declared dependency nor a runtime builtin, and could name a changed path: for a relative specifier, that path with any extension or a directory index under it; for an alias whose target is missing, anything under the target; for any other bare specifier, a file whose path without its extension ends with the specifier. An unresolved import that could name no changed path is not a reason (carrick#2826): a package that changed shows as a `repo_input_changed`. |
| `extends_unfollowed` | A tsconfig `extends` it reads could not be followed. |
| `repo_input_changed` | A lockfile, any `package.json`, `pnpm-workspace.yaml`, any `deno.json` or `deno.jsonc`, any `.gitignore`, `.gitattributes` or `.gitmodules`, any `.npmrc`, `.yarnrc`, `.yarnrc.yml` or `bunfig.toml`, any `*.graphql`, `*.gql` or `*.graphqls`, or `carrick.json` changed. Every service is analysed. Two changes are not one: a `carrick.json` the tree holds untracked that names exactly the stored services, and a `package.json` that differs from the stored copy only in its top-level `version` (that one is read only by the services that import its package). |
| `inputs_changed` | A changed path is under a root, in the closure, or under an input directory, or a path was added or deleted in a directory where one of its imports is looked up. |

Otherwise it is carried. Each service gets one `scope:` log line, followed
by a summary and the closure walk's wall time.

A carried service's type verdicts, SDK edges and boundary are computed
again in the join. It is uploaded only when one of those differs from its
stored blob, stamped with this run's commit, `dirty`, time and build. Every
other carried service keeps its stored row. A run in upload mode that
writes nothing ends with `close-scan`, reason `no_change`, and exits 0.

## Environment

| Variable | What it does |
|---|---|
| `CARRICK_TOKEN` | The credential. Overrides the file `carrick login` writes. |
| `CARRICK_RUN_ID` | The run this scan belongs to, so the scan's log and the runner's join on one key. |
| `CARRICK_RUN_REPO_COUNT` | How many repositories the run covers, so the cloud waits for all of them before it mails. |
| `CARRICK_SIDECAR_MAX_OLD_SPACE_MB` | The type checker's V8 heap cap, in MB. `0` passes no cap. |
| `CARRICK_FAIL_ON_TYPE_MEMORY` | Stop the scan at the first type-checker memory death and exit 3, instead of landing that service without types and exiting 0. |
| `CARRICK_SERVICE_CONCURRENCY` | How many services are analysed at once. `1` is one at a time. Unset, the scan sizes it from free memory and cores, at most 8. A higher number is honoured even when memory would not allow it. |
| `CARRICK_ALLOW_UNPREPARED` | Scan a checkout whose dependencies are not installed instead of refusing it. |
| `CARRICK_PROMPT_ENDPOINT` | Where the four prompt routes (`/analyze-file`, `/generate-intent`, `/framework-detect`, `/framework-guidance`) are sent, instead of the build-time endpoint. `http://127.0.0.1:<port>` or `http://[::1]:<port>` only; anything else is a usage error (exit 2). `/library-claims` and `/types/check-or-upload` keep the build-time endpoint. Headers and bodies are unchanged. |
| `CARRICK_PROMPT_TIMEOUT_SECS` | The per-request timeout of the four prompt routes, in whole seconds, 1 to 900; anything else is a usage error (exit 2). Unset: 60. Other routes keep 60. |

## Clone shape

The folder the checkout lives in can have any name once `--repo` is passed.
For unchanged files to replay their stored answers, the commit each service's
stored blob was written at must be present in the clone.

- A partial clone (`--filter=blob:none --depth=1`) works with that commit
  fetched the same way. The scan's git commands fetch no file contents; a
  tracked file edited after checkout reads as changed and is analysed again
  (`unchanged_since`, `src/git_state.rs`).
- Each service carries the commit of its own stored blob, and the commits
  differ when services landed at different times. Fetch every distinct one
  (`resolve-repos` lists them per service as `hash`), not one.
- A stored commit that no longer exists on the remote, for example after a
  force push, cannot be fetched; that service runs in full.
- A `--pr` scan names the functions the pull request added, edited or
  removed by reading each changed file at the merge base of HEAD and
  `--pr-base`. The clone must hold that merge base and, in a partial clone,
  both copies of every changed file: one `git diff -M <base> HEAD` with its
  output discarded fetches them. The scan reads only what the clone holds
  (git 2.44 or later), so a missing copy leaves the result's `functions` as
  `base_unavailable` and the reason in an info line (`changed_files`,
  `src/git_state.rs`).
- Keep the tree clean before the scan: install with frozen lockfiles where
  the manager allows it. A rewritten tracked file also marks the upload
  `dirty`. With `CARRICK_INSTALL_LOCKFILE_RETRY=1` (the runner sets it), a
  frozen install that fails is retried once without the frozen lockfile and
  the lockfile in the tree is left byte-identical (`scripts/install-scanned-deps.sh`).

## Exit codes

| Code | Meaning |
|---|---|
| 0 | The scan finished. What it still owes (pending services, losses allowed by `CARRICK_ALLOW_PARTIAL_ANALYSIS`, budget refusals) is in its summary. |
| 1 | The scan failed, and the stage was reported to the cloud (`scan-failed` or `preflight-failed`). Includes an unknown argument to the bare scan. |
| 2 | Usage: an unknown command, a local command's parse error, a `--repo` value that is not `owner/name`, `--upload` with `CARRICK_OUTPUT_JSON`, or `--changed-only` with `--no-cache`. |
| 3 | The type checker died of memory and `CARRICK_FAIL_ON_TYPE_MEMORY` was set: it reached its V8 heap limit, or the kernel killed it and the cgroup counted an OOM kill. A death in a service's analysis lands every service whose own type checker lived, and not that one; services not yet started when it died are not started; a death in the cross-service check lands every service without verdicts. The scan was reported as `scan-failed`, with a reason starting `types_out_of_memory:` that names the stage and the heap cap. Run it again on a larger machine (`src/scan_health.rs`, `TypesOutOfMemory`). |
| 101 | Panic. It is first reported as `scan-failed` with reason `panic` (`src/panic_report.rs`). |
| 129 / 130 / 143 | SIGHUP / SIGINT / SIGTERM (`src/shutdown.rs`). 143 is also an ECS `StopTask` or a Spot reclaim. |
| other | Killed from outside. 137 is SIGKILL, which on ECS is usually the task's memory limit: read the stop reason. |
