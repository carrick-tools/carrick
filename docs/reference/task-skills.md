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
set of class words (DUPLICATE, VARIANT, FALSE POSITIVE; MATCH, DRIFT, CONSUMER
UNTYPED, PRODUCER UNTYPED; COMPATIBLE, INCOMPATIBLE, UNRESOLVED, NOT COMPARED).
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

## Ordering against the cloud

The bodies name MCP tools, and a skill that names a tool the deployed server
does not serve is a skill that fails on its first step. `find_similar` and
`get_contract_pair` reach users with the cloud deploy that serves them, so the
release carrying these skills follows that deploy.
