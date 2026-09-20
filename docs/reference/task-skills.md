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
| `carrick-census` | "Every place that does X" questions | `search_by_intent` asked in two wordings, purpose and mechanism, paged to the end |

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

## Classing, and what makes it unskippable

A class word the body defines and nothing applies is a class word the write-up
leaves off the hard rows, which is what carrick#1349 measured. Three properties
hold the class column to the tool's answer, and the skill tests pin them.

Every row the tool returned carries a class, and the column is never empty. In
`carrick-reuse` the rows are the matches of a targeted call and every cluster
member beyond the first, classed against the function asked about or against the
cluster's first member. In `carrick-impact` they are the consumer call sites, and
in `carrick-drift` the operations.

The tests are mechanical and ordered, so the first that holds decides and two
words cannot describe the same row equally well. `carrick-reuse` reads both
spans and takes a different contract as FALSE POSITIVE, one contract with a
behavioural difference it can name in a clause as VARIANT, and one contract with
nothing left to name as DUPLICATE. A copy whose own comment names the file it
mirrors is a VARIANT whose clause is "documented mirror"; `find_similar` carries
no field for that, and the evidence is in the span it points at.
`carrick-drift`, where a stored verdict and an untyped side both fit, takes the
first of DRIFT, PRODUCER UNTYPED, CONSUMER UNTYPED, UNRESOLVED, NOT JUDGED,
MATCH.

A partial write-up is visible in the report. Each body pages its tool to the end
and states rows returned against rows classed, so a table that stops short says
so in its own numbers.

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
| `carrick hook post-edit` | `npm/carrick/src/hook/post-edit.ts`, `npm/carrick/src/hook/apply-patch.ts`, `npm/carrick/src/hook/reuse.ts` | records those names for the session under `~/.carrick/sessions/<session_id>.json`, outside every repository, and prints not one byte about them |
| `carrick hook stop` (Claude Code) or `carrick hook user-prompt` (Codex) | `npm/carrick/src/hook/stop.ts`, `npm/carrick/src/hook/user-prompt.ts`, `drain` in `npm/carrick/src/hook/reuse.ts` | names the accumulated set once and points at this skill |

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

The same three parts run on Codex through a different event and a different
payload reader; that is the section below.

### Codex (carrick#1335)

One implementation, two delivery channels. The recording half, the per-session
store, the line itself and the once-per-set marking are the same code on both
hosts; what differs is the event that carries it and the payload the recorder
reads.

**The event.** Codex accepts `additionalContext` only on `PreToolUse`,
`PostToolUse`, `SessionStart`, `UserPromptSubmit` and `SubagentStart`, and warns
"this event cannot emit additionalContext" for every other one
(`codex-rs/hooks/src/engine/discovery.rs`). Its `Stop` event is therefore not
available to a nudge that must not block, so the nudge is delivered by
`UserPromptSubmit`: `carrick hook user-prompt` drains the same pending set and
prints the same `hookSpecificOutput` object with `hookEventName:
"UserPromptSubmit"` on it. The ruling of 2026-09-20 took this over a blocking
`Stop` hook.

**The limit that follows.** The line arrives with the NEXT prompt, not at the
end of the task that earned it — and a one-shot run with no following prompt
(`codex exec "…"`, a session closed on the nudging task) never sees it at all.
The marking is what makes a late delivery safe: the set is spoken once, whenever
the next prompt comes, and never again.

**The payload.** Codex has no `Edit`/`Write`/`MultiEdit` with a `file_path`. It
edits through `apply_patch`, whose `tool_input` is `{ "command": "<patch text>"
}` (`codex-rs/core/src/tools/handlers/apply_patch.rs`), and one patch can touch
several files. `npm/carrick/src/hook/apply-patch.ts` reads the four header lines
out of it: `Add File` and `Update File` are re-checked, `Delete File` is dropped
because the path is gone once the patch applies, and `Move to` replaces the
`Update File` above it because the destination is the file that now exists. Each
surviving path is re-checked and recorded exactly as a Claude Code edit is.

**The file.** `carrick init` writes `.codex/hooks.json` — Codex's project config
layer (`ConfigLayerSource::Project`), which nests its groups under a `hooks` key
in the same shape a `.claude` settings file does, so the merge, the reader and
the remover are the ones in `settings.ts`. It holds two entries: `PostToolUse`
with matcher `apply_patch` (matchers are regexes) and `UserPromptSubmit`. A file
the user already has is merged into entry by entry; an entry is recognised as
ours by the command it runs, which is the same rule the `.claude` file follows.
`carrick remove` takes those entries out, and deletes the file — and the
`.codex` directory, if it is then empty — when nothing but our entries was in
it. `SCAFFOLD_FILES` in `src/git_state.rs` lists the path, so a first run does
not read its own install as a dirty tree.

**One step init cannot take for you.** A project hook is untrusted until Codex
records its hash, and an untrusted hook is discovered and never run
(`hook_trust_status` in `discovery.rs`). Codex asks at its next start; `carrick
init` says so on the line that reports the file.

`carrick doctor` reports a missing or outdated Codex entry the way it reports
the Claude ones, and only where the workspace holds a `.codex/` directory — that
folder is Codex's project config layer and the one init writes into, so its
absence means Codex is not set up here and doctor says nothing about it.

## Ordering against the cloud

The bodies name MCP tools, and a skill that names a tool the deployed server
does not serve is a skill that fails on its first step. `find_similar` and
`get_contract_pair` reach users with the cloud deploy that serves them, so the
release carrying these skills follows that deploy.

A field is a softer case than a tool, because the package can reach a user on
either side of the deploy that serves it, so a body reads what its answer
carries rather than asserting a shape. `carrick-reuse` takes `vector_basis` from
the answer instead of assuming which of the two stored vectors the cosines run
over, and relays whatever keys `not_compared` and `excluded` hold rather than a
list written here. `carrick-census` asks both wordings on one call with
`also_phrased_as` and falls back to a call per wording where the answer carries
no `phrasings`, and it reports `hidden_by_threshold` where the answer carries
it.
