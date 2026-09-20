# Task skills

The four skills `carrick init` installs, what each one does, and when it fires.

Governed code: `npm/carrick/src/init/task-skills.ts` (the install, the stamp and
the removal) and `npm/carrick/templates/skills/` (the bodies, one markdown file
each).

A skill here performs a task. Every step that finds something is a Carrick tool
call or a `carrick check` run, and the agent's work is to confirm what came back
in source, classify it, decide and act. A body that asks the agent to go and
explore instead is not one of these.

The reminder skill at `.claude/skills/carrick/SKILL.md` is the cloud scaffold's
and is untouched by everything below.

## The four

| Skill | When it fires | What it calls |
|---|---|---|
| `carrick-impact` | Before changing or removing a route, a handler, a response shape, an event, or a function other code calls. Also on "who calls this" and "what breaks if I change it" | `get_operation`, `get_callers`, `check_compatibility`, and `carrick check <file> --recheck --json` for a file already edited in the working tree |
| `carrick-reuse` | At the end of a task that added or changed functions, and on "does this already exist" or "where have we built this twice" | `find_similar`, targeted with up to 20 entries, or in audit mode with none |
| `carrick-drift` | Before changing a request or response type, and when a compatibility verdict names a problem with no location | `get_service_graph` for the pairs, then `get_contract_pair` per pair |
| `carrick-census` | "Every place that does X" questions | `search_by_intent` twice, once worded by purpose and once by mechanism, paged to the end |

Each one reports a table first, with a file and line on every row and a fixed
set of class words (DUPLICATE, VARIANT, FALSE POSITIVE; MATCH, DRIFT,
UNRESOLVED, NOT JUDGED, CONSUMER UNTYPED, PRODUCER UNTYPED; COMPATIBLE,
INCOMPATIBLE, UNRESOLVED, NOT COMPARED). Every verdict state a tool can return
carries a class, because a state with no class is a finding the skill drops. A
`carrick-drift` operation whose stored verdict is `unresolved`, or which carries
no verdict at all, is classed, and its two type texts are read against each
other and reported as "type texts differ" rather than as a verdict.
Each relays the counts its tool stated about what it did not look at. None of
them edits code unless asked, and each ends by offering one issue per finding.

## Where they are written

`carrick init` writes both harnesses' copies, with identical bytes:

```
.claude/skills/<name>/SKILL.md
.agents/skills/<name>/SKILL.md
```

Where init has settled a Carrick project, its slug is written into every tool
call in the bodies. Where no repository in the workspace names a GitHub
repository, no project is settled and the bodies tell the agent to read
`git remote get-url origin` and pass `repo: "<owner/repo>"` instead.

A skills directory this repository ignores works for the machine that ran
`carrick init` and for nobody else, so init says so once and changes nothing.

## The stamp

Every file ends in a marker and a digest of the body above it:

```
<!-- carrick:skill sha256:0123456789ab -->
```

The digest, not a version number, is what separates the three states a path can
be in on a re-run. A file whose digest still matches is one this package wrote
and nobody has touched since, whichever version wrote it, and a new version
writes over it. A file carrying the marker over a body that has moved has been
edited here, and one carrying no marker was never ours; both are left where they
are and named on the terminal.

`carrick remove` deletes the files whose digest still matches, and names the
rest for a human. It is the one thing `carrick remove` deletes from a repository
rather than printing a `git rm` line for, and the digest is why: those bytes are
reproducible from this package and hold nobody's work.

`SCAFFOLD_FILES` in `src/git_state.rs` lists these eight paths, so a first run
does not read its own install as a dirty tree. A skill added or renamed here is
added there in the same change.

## The reuse nudge (carrick#1330)

`carrick-reuse` is the one skill with a moment that can be detected rather than
remembered, so it is the one a hook points at. Measured before it was built:
prose alone reached the index in none of five runs, and a hook pack in five of
five. A reuse check that waits to be remembered does not run.

Three parts, in the order they fire:

| Part | Governed code | What it does |
|---|---|---|
| `carrick check <file> --recheck` | `src/local_mode/recheck.rs` | already re-extracts the edited file; now also answers with `recheck.new_functions`, the functions it declares that the index does not hold (`docs/local-mode-output.md`) |
| `carrick hook post-edit` | `npm/carrick/src/hook/post-edit.ts`, `npm/carrick/src/hook/reuse.ts` | records those names for the session under `~/.carrick/sessions/<session_id>.json`, outside every repository, and prints not one byte about them |
| `carrick hook stop` | `npm/carrick/src/hook/stop.ts` | at the end of the task, names the accumulated set once and points at this skill |

Why the nudge is not on the edit: most edits add no function, and every nudge
costs a model turn. Why it is not left to the agent: see the measurement above.

**The channel is the feature.** A Stop hook's
`hookSpecificOutput.additionalContext` is delivered to the model and the
conversation continues, which is the one Stop channel that is both model-visible
and non-blocking. `systemMessage` is shown to the user and never reaches the
model — a Stop hook of ours fired in 21 sessions through that field and changed
nothing — and `decision: "block"` reaches the model by refusing to let the turn
end, which is a block. The hook emits `hookSpecificOutput` and nothing else.

**It fires once per set.** The names a nudge has spoken are marked in the same
file, before the line is written, so the next stop of the same session says
nothing about them. A function added after a nudge is pending on its own.

**It fires even when the index is older than the branch point**, and states the
commit it compared against in the line (the ruling of 2026-09-20 on open
question 2). The two limits are in the text the model reads, because they decide
what an empty answer means: a function added on this branch is compared against
the default branch as the last scan saw it, and the comparison is on what each
function is described as doing, not on its source.

`carrick remove` deletes the session records; `carrick doctor` reports a missing
Stop entry with the other two, because both read `expectedCarrickHooks`.

### Codex

Codex reads the same hook manifest shape from `$CODEX_HOME/hooks.json` and from
a project's `.codex/hooks.json`, and it has a `Stop` event. It cannot carry this
nudge, for two reasons that are in its source:

1. `additionalContext` is accepted only on `PreToolUse`, `PostToolUse`,
   `SessionStart`, `UserPromptSubmit` and `SubagentStart`
   (`codex-rs/hooks/src/engine/discovery.rs`, which warns "this event cannot
   emit additionalContext" for every other event). A Codex `Stop` hook reaches
   the model only through `decision: "block"` and its continuation prompt, which
   is a block.
2. The recording half would need its own payload reader: Codex's edit tool is
   `apply_patch` with its own `tool_input`, not `Edit`/`Write`/`MultiEdit`
   (`codex-rs/hooks/src/events/post_tool_use.rs`).

`carrick init` also writes no Codex hook file today — only `.agents/skills/` —
so shipping this for Codex means a new file as well as a different trigger. The
options are a blocking Stop hook, or a non-blocking `UserPromptSubmit` hook that
delivers the nudge one turn late. Tracked as carrick#1335.

## Ordering against the cloud

The bodies name MCP tools, and a skill that names a tool the deployed server
does not serve is a skill that fails on its first step. `find_similar` and
`get_contract_pair` reach users with the cloud deploy that serves them, so the
release carrying these skills follows that deploy.
