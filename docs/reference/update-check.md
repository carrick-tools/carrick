# Staying on the current Carrick

Governs `npm/carrick/src/update.ts`, `npm/carrick/src/update-check.ts`, the
`noticeUpdate()` block in `npm/carrick/bin/carrick.mjs`, the version line in
`npm/carrick/src/hook/session-start.ts`, and `checkVersion` /
`pinnedActionRef` in `npm/carrick/src/init/doctor.ts`.

## The problem

A pilot customer was asked to upgrade before re-running. His agent did not
upgrade, he ran a stale version, and the scan failed on a defect that was
already fixed. On one morning he was running two different versions, and on
the older one most of the framework-detect calls exhausted their retries — a
failure mode that did not exist on the newer build. The upgrade path was "we
ask the human, the human asks their agent, the agent forgets."

David's framing:

> There must be a way people do this — i.e. start of session, not mid
> session. CI auto updates the carrick action, but now we have the package I'm
> not sure how that is handled.

## What was actually pinned, and what was not

**The GitHub Action already floats.** `action.yml` reads the version out of
the `Cargo.toml` sitting beside it in the action checkout, and `release.yml`
force-moves the `v1` tag onto every release commit. A workflow that says
`carrick-tools/carrick@v1` — which is what `carrick templates workflow` and
`carrick init` write — therefore picks up each release with nobody touching
anything. Nothing needed fixing there.

A workflow that pins anything else (`@v1.4.2`, a branch, a commit SHA) is
frozen at whatever shipped with that ref, for as long as the workflow stands,
and `carrick doctor` used to read a pinned ref back as a variable and report
no finding at all. `pinnedActionRef` is that finding.

**`npx --yes carrick@latest` is not the problem either**: it resolves fresh on
every invocation. A bare `npx carrick` is — npx reuses its cached tree.

**An installed `carrick` is the real case.** `npm install -g carrick` is what
the README, the plugin docs and `carrick init` all tell people to run, and
from that moment the version on that machine never moves. The same is true of
a `carrick` devDependency. This is what bit the customer and it is what this
subsystem exists for.

## Notify, do not self-update

Two camps in the ecosystem. Notify-and-name-the-command: npm (update-notifier),
`gh` (a cached check plus `GH_NO_UPDATE_NOTIFIER`), pnpm, deno, uv, rustup,
bun, wrangler, vercel — all of them print, and all of them keep the upgrade as
a separate explicit act. Self-mutating: Homebrew, Claude Code.

The dividing line is not taste, it is whether the tool owns its install
location. Homebrew owns a prefix; Claude Code ships a native installer for
exactly this reason. Carrick does not own anything: it can be under `npm -g`,
`pnpm -g`, `bun -g`, a Volta shim, an npx cache, one global per nvm-managed
Node, or a project's `node_modules`. A self-update that guesses the shape
wrong does not fix a stale install — it adds a second one beside it, and PATH
picks whichever it likes. That is precisely the "two versions on one machine
in one morning" incident, not a fix for it.

So: **detect the shape and print the exact command for it.** An agent that
reads "Update with `pnpm add -D carrick@latest`" runs that command; the value
is in getting the command right, which a self-updater needs to do anyway and
which fails silently when it is wrong.

An explicit `carrick upgrade` that runs the detected command is a small
follow-up on top of `installShape`, not a prerequisite.

That argument still holds for the notice, and "Global sync" below is where it
stops holding. It is not a self-update: a run through npx does not replace
itself, it replaces the OTHER carrick it can prove is there — resolved off
PATH, with its shape read from where it actually sits, and with the result
verified by resolving it again. The failure the paragraph above describes is a
guess about where an install is; that one does not guess.

## Where the check runs, and what it costs

| Surface | When | Cost |
|---|---|---|
| `bin/carrick.mjs`, every command but `lsp` and `hook` | start of the command, stderr | one small file read; a detached child at most once per TTL |
| `bin/carrick.mjs`, `lsp` and `hook` | refreshes the cache, prints nothing | a detached child at most once per TTL |
| `hook session-start` | start of the session, **stdout** | the same cached read |
| `carrick doctor` | on demand, a finding | one bounded fetch when the cache is stale |
| any command, in CI, in front of a scan | before the scan, as a workflow annotation | one fetch bounded to 2 s |

The laptop path never blocks on the network. The invocation reads the cache
and prints from it; a detached child fetches the registry's `dist-tags` and
writes the answer for the NEXT invocation. That costs one run of lag after a
release and zero milliseconds on the path a person waits on — the same model
update-notifier uses.

The parent stamps the cache with the current time *before* it spawns the
child, keeping whatever `latest` it already knew. That stamp is what makes a
dead network cheap: the child can fail silently and nothing forks another
until the TTL comes round. A stamp that cannot be written is the signal to
spawn nothing at all, so a read-only config directory forks a child exactly
never.

CI is the exception, because a build machine is thrown away at the end of the
job and would never get a second invocation to read its own cache. There the
check is synchronous, bounded at two seconds, and runs only in front of a scan
— two seconds in front of work that takes minutes, and not in front of
`carrick status`. It emits a `::warning::` annotation and **continues on the
old version**. CI's contract is that the workflow decides what runs; a scanner
that refused to start because a newer one exists would be a worse outage than
the stale build.

The annotation names one of two different fixes, and naming the wrong one sends
a reader to a line their workflow does not contain. The runner sets
`GITHUB_ACTION_REPOSITORY` for a step belonging to an action, so a run through
the Carrick action is told to move its `uses:` ref to `@v1`; a workflow that
calls the CLI directly wrote a version string, and that is what it changes.

The notice itself never mentions `CARRICK_NO_UPDATE_CHECK`. The session-start
line goes on stdout, straight into an agent's context, and the failure this
subsystem exists for is an agent that does not upgrade — handing it an off
switch in the same sentence as the fix invites the wrong one. The variable is
documented in `carrick --help` and in the package README, where a person reads
it.

## Fail open, always

Every one of these ends in silence and an unchanged exit code: no network, a
non-2xx, a rate limit, a body that is not JSON, a `latest` that is not a
version, an absent cache, a corrupt cache, a cache of the wrong shape, a
config directory that cannot be written, a clock that moved backwards, a
version string this build cannot parse. `test/update.test.ts` has an assertion
for each.

`isNewer` requires a plain `x.y.z` on both sides, so nobody is ever nudged
onto a prerelease and a binary built from a checkout is left alone. Note that
`release.yml` publishes without `--tag`, so a prerelease would land under
`latest` if one were ever cut; release-please's configuration does not produce
one, and this guard is the second line rather than the first.

`CARRICK_NO_UPDATE_CHECK=1` turns all of it off, for a reproducible run or an
air-gapped machine. The package's own test script sets it through
`test/no-update-check.mjs`, because a third of the tests spawn the shim as a
real child process.

## Global sync: an npx run leaves no older carrick on PATH (carrick#1372)

The notice above still installs nothing. This is the one thing beside it that
does, and only ever to an install that already exists.

`npx carrick@latest` is current by construction, and a global install from
three releases ago goes on answering everything else on that machine: every
agent hook (`carrick hook post-edit` is resolved on PATH), every script, every
new shell. That is the "two versions on one machine in one morning" incident,
seen from the other side — the run a person watched was fine and everything
after it was not.

So a command started through npx, before it does anything else:

1. Resolves `carrick` on PATH, skipping the npx cache it is running from, and
   reads that install's version off its own manifest.
2. Where that is older, upgrades it to the version this run is, with the
   command for the shape it is in (`npm install -g`, `pnpm add -g`,
   `bun add -g`, `volta install`) and **no flag of ours**: not a registry, not
   a prefix, not a release-age override. Somebody who installed carrick
   globally chose to have it on the machine; keeping that choice current is
   inside what they agreed to. Forcing it past their machine's policy is not.
3. Resolves `carrick` again and compares. A second install earlier on PATH, a
   version manager's shim, or an install that reported success and changed
   nothing all end the run with a line naming the path that is still stale and
   the command for it. It never ends silently on an older binary.

A refusal is printed with its reason and the one command, and the run carries
on. Nothing is retried with more force.

`hook`, `lsp`, `remove`, `--version` and `--help` are excluded, along with CI
and `CARRICK_NO_UPDATE_CHECK=1`. A run that is not through npx is skipped
because it IS the install in question: a global `carrick index` is already the
version on PATH, and a project dependency belongs to the repository that pins
it — that one is reported, never replaced.

### A machine with no global

Never installed onto silently. Interactive `carrick init` asks;
`carrick init --yes` prints the command and installs nothing, because `--yes`
is an answer about the workspace; `carrick init --install-global` is the
consent a script gives. The answer decides what the hook entries say: with a
global they are the bare `carrick`, and without one they name the install that
wrote them — which, under npx, is a cache directory that gets cleared.

`init` used to write the bare command whenever `which carrick` succeeded, and
inside an npx run that is always: npm puts the exec tree's own
`node_modules/.bin` first on PATH for the child it runs. Every hook entry
written that way named a command that stopped existing when npx exited.
`findCarrick` walks PATH itself and skips that entry.

### Release-age policy, measured

npm documents that a project `.npmrc` is not read in global mode. Measured
with npm 12.0.2 against `carrick@0.3.84`, published the day before:

| Where `min-release-age=43200` was set | `npm install -g carrick@0.3.84 --dry-run` |
|---|---|
| the project `.npmrc` in the cwd | installed, 37 packages |
| the user config (`--userconfig`) | refused: `code ETARGET … no matching version … with a date before …` |

So a repository's release-age policy does not block this, and a person's own
does — which is a refusal they see, with its reason and the command, rather
than something overridden behind their back. The pair runs as a step in the
`Global install` job, with the second half asserted: an npm too old to know
the setting would install in both and turn that step red.

### The backstop

Each hook compares the version it is running with the version recorded in
`.carrick/cli-version` when `carrick init` wrote these files, and says so on a
mismatch — upgrade the install when the hook is older, run `carrick init` when
it is newer. `carrick doctor` reports the same comparison against the `carrick`
a PATH lookup finds. A version number rather than content, unlike everything in
`outdated.ts`: the question is which build is answering, not whether a body
changed. It lives in `.carrick/`, which is ours and ignored, and never in a
settings file or an MCP entry (carrick#1333).

Session start says it once per session, unthrottled. The hooks that fire on
every edit say it once a day per workspace.

## What this does not touch

Nothing in Rust, so no `CACHE_VERSION` and no `INTENT_CACHE_VERSION`. No
prompt bytes, no analysis cache key, no index blob field, no wire envelope, no
cloud deploy. The registry is the only thing consulted, and the cloud is not
asked anything it was not already asked.
