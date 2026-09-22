// Putting the hosted index on this machine, at the end of `carrick init`.
//
// The second developer on a team clones a repo whose index CI already built,
// and until this ran they were told not to run the only command that would
// give them a local one: `carrick status`, the hooks and the language server
// all answered "no local index" straight after a successful setup
// (carrick#1020).
//
// So init ends by populating `.carrick/` from the hosted index. `carrick
// refresh` is the pass that does it: it re-reads this workspace's source
// deterministically and replays the hosted rows onto it, and it runs no model
// and uploads nothing, so it costs nothing and replaces nobody's row.
//
// The re-read is the expensive half and the hosted fetch is the small one:
// the pass is minutes on a large workspace and the download is two requests
// (carrick#1373). So this asks `carrick status` FIRST and runs the pass only
// when the answer says it would change something — no index here, source that
// has moved since the index was built, a repo the index does not cover, or a
// hosted read this machine has not yet made signed in and connected. On a
// workspace `carrick index` has just indexed, nothing is re-read.
// carrick#985 is the other half of that saving, inside `refresh`: a repo whose
// local blob is already current should be fetched for rather than scanned.
//
// What it reports is read back from `carrick status`, not assumed from an
// exit code: a refresh that scanned fine can still have been refused the
// hosted rows, and the sentence a reader needs is different in each case
// (carrick#1012).

import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import type { RunningScan, StatusRepo, StatusResult } from "../contract.ts";
import { parseStatusResult } from "../contract.ts";
import { nativeEnv, resolveNativeBinary } from "../native.ts";
import { elapsed, parseMarker } from "../scan.ts";
import type { StepProgress, StepReport } from "./output.ts";

/**
 * One run of the scanner binary, injected so tests spawn nothing.
 *
 * `watch` asks for the run's progress markers: the re-read is the one call
 * here that takes minutes, and `status --json` is a read whose output is the
 * answer rather than a thing to draw.
 */
export type NativeRun = (
  args: string[],
  watch?: boolean,
) => Promise<{
  status: number | null;
  stdout: string;
  stderr: string;
}>;

export type HostedDownload =
  /** The hosted rows are on this machine, for `services` services. */
  | { kind: "downloaded"; services: number; reread: boolean }
  /** The hosted blob predates this CLI, so its rows were not replayed. */
  | { kind: "version_mismatch"; services: number; reread: boolean }
  /** There is a local index, without the hosted rows: `state` says why. */
  | { kind: "local_only"; services: number; state: string; reread: boolean }
  /** A scan is building this workspace's index right now, so this left it alone. */
  | { kind: "scanning" }
  /** There is no local index at all, and `problem` is why. */
  | { kind: "failed"; problem: string };

/**
 * What `carrick status` says about the index in `.carrick/` before this step.
 *
 * The `reread` case carries the sentence for WHY the pass has to run. That
 * pass is the wait a reader is about to sit through, and the label alone does
 * not say which case they are in (carrick#1373).
 */
export type LocalIndexState =
  /** Nothing a re-read would change: the pass is skipped. */
  | { kind: "current" }
  /** Another scan owns this index right now; a second pass would contend with it. */
  | { kind: "scanning" }
  /** The pass has to run, and this is what makes it worth the minutes. */
  | { kind: "reread"; reason: string };

/**
 * The hosted states a re-read can still change, so an index carrying one is
 * not current however clean the tree is.
 *
 * All four are "this machine never got a usable answer for a reason this run
 * has just addressed": `carrick init` signs in, connects the repos and can put
 * them in a project, and the recorded state predates all of it. The states
 * left out are the ones a second read answers the same way — `enriched` is
 * done, and `version_mismatch`, `read_failed` and `commit_missing` are the
 * hosted blob's own condition, which nothing here moves (carrick#1024 is the
 * dirty-blob one).
 */
const ANSWERED_BY_A_REREAD = new Set([
  "not_signed_in",
  "not_connected",
  "remote_unnamed",
  "no_index_yet",
]);

/**
 * Whether the index this machine holds is already the one a re-read would
 * write.
 *
 * Every input is on the status answer already, so this costs one read of a
 * file: `changed_since_index` per service and `outside_every_service` per repo
 * are the scanner's own count of source that has moved since the index was
 * built, and both are filtered to files a scan reads — which matters here,
 * because `carrick init` writes `carrick.json`, the hooks and the skills, and
 * none of those is a reason to spend minutes (carrick#1007 item 5).
 *
 * `repos` is the workspace's repo list as `carrick derive` gave it, so a repo
 * this folder holds and the index does not is a re-read and not a silent gap.
 * Paths are compared as the filesystem resolves them: both sides descend from
 * the same `--workspace` argument, and a pair that still fails to line up
 * falls to the slow side, which is the safe one.
 */
export function localIndexState(status: StatusResult | null, repos: string[]): LocalIndexState {
  if (status === null) return { kind: "reread", reason: "there is no index here yet" };
  // Before the index is read at all: a detached `carrick index` writes this
  // one, and a second pass over the same tree would be two scans contending
  // (carrick#992).
  if ((status.running_scans ?? []).some(stillScanning)) return { kind: "scanning" };
  if (status.error !== undefined || status.services.length === 0) {
    return { kind: "reread", reason: "there is no index here yet" };
  }
  const changed = status.services.reduce((total, service) => total + (service.changed_since_index ?? 0), 0) +
    (status.repos ?? []).reduce((total, repo) => total + (repo.outside_every_service ?? 0), 0);
  if (changed > 0) {
    return { kind: "reread", reason: `${changed} file(s) changed since it was built` };
  }
  const indexed = new Set((status.repos ?? []).map(resolved));
  const uncovered = repos.filter((repo) => !indexed.has(resolved(repo)));
  if (uncovered.length > 0) {
    return { kind: "reread", reason: `it covers ${uncovered.length} fewer repo(s) than this folder holds` };
  }
  const unasked = status.services.some(
    (service) => service.hosted_state !== undefined && ANSWERED_BY_A_REREAD.has(service.hosted_state),
  );
  if (unasked) return { kind: "reread", reason: "it holds no hosted rows yet" };
  return { kind: "current" };
}

/**
 * Whether a scan row is a process that still holds this workspace.
 *
 * Two things have to be true, and the recorded status is only the first of
 * them. The list also carries the scans that finished, failed or handed their
 * prompts to Carrick Cloud, and none of those is holding anything — and a
 * `running` row OUTLIVES its process: `forget_superseded` clears it on the
 * next build, not when the scan ends, so a scan somebody interrupted leaves
 * one behind. The scanner answers this with `kill(pid, 0)` everywhere it
 * matters (`ScanState::is_running` in `src/local_mode/scan_state.rs`) and
 * serialises the raw status, so this asks the same question of the same pid.
 * A row with no readable pid is not a scan this can prove is alive, and the
 * cost of being wrong that way is one re-read rather than an init that never
 * re-reads again.
 */
function stillScanning(scan: RunningScan): boolean {
  if (scan.status !== "running" || typeof scan.pid !== "number") return false;
  try {
    // Signal 0 performs no action; it only reports whether the pid can be
    // signalled. EPERM is a live process this user does not own.
    process.kill(scan.pid, 0);
    return true;
  } catch (error) {
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

/** A path as the filesystem resolves it, for comparing two sides' spelling of one repo. */
function resolved(entry: string | StatusRepo): string {
  const value = typeof entry === "string" ? entry : entry.repo;
  try {
    return fs.realpathSync(value);
  } catch {
    return path.resolve(value);
  }
}

/**
 * Run the scanner, letting its own progress reach the terminal.
 *
 * A refresh of a large workspace is minutes, and a silent minute reads as a
 * hang, so the scanner's stderr — which is where it says what it is indexing —
 * is written on as it arrives as well as kept. Kept, because the last of it is
 * the reason a failed read failed, and that reason is this command's to state
 * in a sentence of its own. Its stdout is the index map, which this command
 * summarises in its own words, so that is captured and dropped.
 */
function spawnNative(
  args: string[],
  quiet: boolean,
  progress: StepProgress | null,
): Promise<{ status: number | null; stdout: string; stderr: string }> {
  const native = resolveNativeBinary();
  if (!native.binary) {
    return Promise.resolve({
      status: 1,
      stdout: "",
      stderr: native.problem ?? "The Carrick scanner is not installed.",
    });
  }
  const watching = progress !== null;
  return new Promise((resolve) => {
    const child = spawn(native.binary as string, args, {
      // The markers are written only when a parent asks for them, so a run
      // nobody is watching is unchanged (`src/progress.rs`).
      env: watching ? { ...nativeEnv(), CARRICK_PROGRESS: "1" } : nativeEnv(),
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    let rest = "";
    const report = progress === null ? null : downloadProgress(progress);
    // The clock half of the cadence (carrick#1373). The hosted request in
    // front of the markers is one call that can run for minutes, so a run
    // driven by markers alone is silent through exactly the wait that needs a
    // line. Unref'd: a ticker is not a reason for the process to stay up.
    const beating = report === null ? null : setInterval(report.beat, BEAT_MS);
    beating?.unref();
    const settle = (value: { status: number | null; stdout: string; stderr: string }): void => {
      if (beating !== null) clearInterval(beating);
      resolve(value);
    };
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => {
      // Bounded: a scan of a large workspace prints thousands of lines, and
      // all this holds them for is the sentence at the end.
      stderr = `${stderr}${chunk}`.slice(-8192);
      if (report !== null) {
        const lines = (rest + chunk).split("\n");
        rest = lines.pop() ?? "";
        for (const line of lines) report.read(line);
      }
      // Quiet under a spinner: a spinner owns its line and rewrites it, and a
      // scanner line landing in the middle of that is a corrupted terminal.
      // Where there is no spinner — a pipe, CI, an agent shell — the progress
      // is the only sign a minutes-long read is alive (carrick#1021). The
      // marker lines are the spinner's own input and are never written through.
      if (!quiet && !args.includes("--json")) process.stderr.write(withoutMarkers(chunk));
    });
    child.on("error", (error) => settle({ status: 1, stdout: "", stderr: error.message }));
    child.on("close", (code) => settle({ status: code, stdout, stderr }));
  });
}

/** A chunk with the machine-readable lines taken out of it. */
function withoutMarkers(chunk: string): string {
  if (!chunk.includes("@carrick-")) return chunk;
  return chunk
    .split("\n")
    .filter((line) => parseMarker(line) === null)
    .join("\n");
}

/**
 * What the download says while it runs: how far through, and how long so far.
 *
 * Read off the scanner's own markers rather than timed here, because only the
 * scanner knows what it is on (`src/progress.rs`). Two counts arrive: the
 * services of this workspace it has finished, which is the one a reader asked
 * for, and the files inside the one it is on, which is the one that moves.
 *
 * No estimate of what is left. An estimate needs work remaining and a measured
 * rate for it, and neither is available here: the hosted half is two requests
 * for the whole workspace, so there are no bytes remaining to divide, and the
 * local half is a re-scan whose services differ in size by two orders of
 * magnitude (carrick#1365). A number that cannot be derived is not shown.
 */
export function downloadProgress(say: StepProgress, now: () => number = Date.now): DownloadProgress {
  const startedAt = now();
  let services = "";
  let bytes = "";
  let saidAt: number | null = null;
  // One line at a time, and never two inside one beat. Both callers below can
  // reach this in the same second — a marker arrives, the ticker fires — and a
  // reader on a pipe gets the cadence, not every marker a large workspace
  // raises (carrick#1315).
  const emit = (): void => {
    const at = now();
    if (saidAt !== null && at - saidAt < BEAT_MS) return;
    saidAt = at;
    // `re-reading your code` on every line, because that is what the minutes
    // are going on: the hosted half is two requests and the pass in front of
    // it reads every file in the workspace (carrick#1373).
    const where = services === "" ? "" : `, ${services}`;
    say([STEP_LABEL, `${REREADING}${where}${bytes}, ${elapsed((at - startedAt) / 1000)}`].join(": "));
  };
  return {
    read: (line: string): void => {
      const marker = parseMarker(line);
      if (marker === null) return;
      if (marker.kind === "progress") {
        const update = marker.update;
        services = `${update.service_index} of ${update.service_total} services`;
        emit();
        return;
      }
      // The hosted half says how much it read; the scanner states it as a
      // notice because it is one fact, once, not a count that ticks.
      if (marker.kind === "notice" && marker.text.startsWith(HOSTED_BYTES)) {
        bytes = `, ${marker.text.slice(HOSTED_BYTES.length)}`;
        emit();
      }
    },
    beat: emit,
  };
}

/**
 * The two ways the download's line is written: off the scanner's own markers,
 * and off a clock.
 *
 * The clock is the half carrick#1373 is about. The markers arrive when the
 * scanner finishes a service, and the hosted request in front of them is one
 * call that can take minutes on a large project — so a run driven by markers
 * alone says nothing at all through exactly the wait a reader needs told
 * about. An agent harness with a tool timeout cannot tell that wait from a
 * hang, and it is the silence that ends the run.
 */
export type DownloadProgress = {
  /** One line of the scanner's stderr. */
  read(line: string): void;
  /** The clock: say where it has got to, whether or not anything moved. */
  beat(): void;
};

/**
 * How often the download says where it is, at most and at least.
 *
 * At most, because the markers of a large workspace would otherwise be
 * thousands of lines; at least, because a reader on a pipe reads silence as a
 * hang. One constant for both halves is what makes the cadence a promise
 * rather than an average.
 */
export const BEAT_MS = 5000;

/**
 * The label the step carries, and the stem of every line it writes.
 *
 * Not "Downloading": the download is two requests, and everything else this
 * step can spend is a local re-read of the tree (carrick#1373).
 */
export const STEP_LABEL = "Putting your index on this machine";

/** What the local half of the step is doing, in the words it costs a reader. */
export const REREADING = "re-reading your code";

/** The notice prefix the scanner states the hosted read's size under. */
export const HOSTED_BYTES = "hosted bytes ";

/** The first line of a failure, for a sentence that has to fit on one. */
function firstLine(text: string, fallback: string): string {
  const line = text
    .split("\n")
    .map((entry) => entry.trim())
    .find((entry) => entry.length > 0);
  return (line ?? fallback).replace(/^carrick(?: \w+)?: /, "");
}

/**
 * A runner for the scanner, told whether the terminal is somebody else's.
 *
 * `quiet` is true while a spinner is showing: a spinner owns its line, and the
 * scanner's progress landing in the middle of it corrupts the terminal. Without
 * one — a pipe, CI, an agent's shell — the progress is the only sign a
 * minutes-long read is alive (carrick#1021, carrick#1026).
 */
export function nativeRunner(quiet: boolean, progress: StepProgress | null = null): NativeRun {
  return (args, watch = false) => spawnNative(args, quiet, watch ? progress : null);
}

/**
 * Put the hosted index on this machine, and say what is there afterwards.
 *
 * `workspace` is the derived workspace root — the directory `carrick init`
 * wrote the proposal into — so the index this reads is the one every later
 * read of that workspace finds. `repos` is that workspace's repo list, which
 * [`localIndexState`] needs to tell "indexed" from "indexes some of this".
 *
 * The status read comes first and decides whether the pass runs at all: the
 * pass is a full local re-read, minutes on a large workspace, and a workspace
 * `carrick index` has just indexed gets nothing out of a second one
 * (carrick#1373). `say` states which case this is in, before the wait rather
 * than after it.
 */
export async function downloadHostedIndex(
  workspace: string,
  run: NativeRun = nativeRunner(false),
  repos: string[] = [],
  say: StepProgress | null = null,
): Promise<HostedDownload> {
  const before = await readStatus(workspace, run);
  const local = localIndexState(before, repos);
  if (local.kind === "scanning") return { kind: "scanning" };
  if (local.kind === "current") return classify(before, null, false);
  say?.(`${STEP_LABEL}: ${local.reason}, ${REREADING}`);
  const refreshed = await run(["refresh", "--workspace", workspace], true);
  if (refreshed.status !== 0) {
    return { kind: "failed", problem: firstLine(refreshed.stderr, "the scanner gave no reason") };
  }
  return classify(await readStatus(workspace, run), refreshed.stderr, true);
}

/** `carrick status --json`, parsed, or null when there is nothing to parse. */
async function readStatus(workspace: string, run: NativeRun): Promise<StatusResult | null> {
  const answer = await run(["status", "--workspace", workspace, "--json"]);
  // Not gated on the exit code: a workspace with no index answers non-zero
  // with the refusal in the body, and that body is what says so (carrick#1023
  // item 1).
  return parseStatusResult(answer.stdout);
}

/**
 * What this machine holds, read off the status answer.
 *
 * `reread` is whether the pass ran, which every sentence below turns on: the
 * same rows are worth a different sentence when the minutes were spent and
 * when they were not. `problem` is the scanner's last words, for the one case
 * where the status answer itself is the thing that is missing.
 */
function classify(status: StatusResult | null, problem: string | null, reread: boolean): HostedDownload {
  if (!status || status.services.length === 0) {
    return {
      kind: "failed",
      problem: firstLine(
        status?.message ?? problem ?? "",
        "the index it wrote holds no service this build can read",
      ),
    };
  }
  const services = status.services.length;
  // Stated before the count, because it is the one state with an instruction
  // attached: a blob older than this CLI is not replayed at all (carrick#1012).
  if (status.services.some((service) => service.hosted_state === "version_mismatch")) {
    return { kind: "version_mismatch", services, reread };
  }
  const enriched = status.services.filter((service) => service.hosted_state === "enriched");
  if (enriched.length === 0) {
    const state = status.services.find((service) => service.hosted_state)?.hosted_state ?? "unknown";
    return { kind: "local_only", services, state, reread };
  }
  return { kind: "downloaded", services: enriched.length, reread };
}

/**
 * Why a hosted index this machine asked for is not in the answer.
 *
 * The state tag is the index's own word for it and reads as jargon in a
 * terminal, so each one is a clause a reader can act on. The reason BEHIND a
 * failed read — a hosted index built from a dirty tree, a commit this clone
 * does not have — is per service and `carrick status` states it, which is why
 * that is where this points.
 */
function stateClause(state: string): string {
  switch (state) {
    case "read_failed":
      return "the hosted rows could not be replayed onto this checkout";
    case "commit_missing":
      return "the commit the hosted index was built at is not in this clone, so run git fetch";
    case "no_index_yet":
      return "the hosted index has not landed yet";
    case "not_connected":
      return "these repos are not connected to a Carrick project";
    // Said of a repo this run may well have just connected: the scanner
    // derives the name again from the git remote, and an origin whose path is
    // not owner/repo names nothing to derive (carrick#1056).
    case "remote_unnamed":
      return "the git remote here names no owner/repo, so the hosted index was not asked for";
    case "not_signed_in":
      return "this machine is not signed in, so the hosted index was not read";
    default:
      return `the hosted rows were not replayed (${state})`;
  }
}

/**
 * The one line init prints about it, and which marker it carries.
 *
 * Every branch states what happened and what this machine now holds. None of
 * them forbids a command: the sentence that did told the reader not to run the
 * only thing that would have given them an index (carrick#1020). A state with
 * an instruction attached is a warning rather than a done line, and a read that
 * produced nothing is a refusal (carrick#1026).
 *
 * This is the step's report, so it is also the line the spinner stops on: one
 * line for the hosted read, in a terminal as everywhere else (carrick#1032).
 *
 * Whether the pass ran is on every line that has two versions. A run that
 * skipped it took a second and a run that did not took minutes, and a reader
 * who cannot tell them apart cannot tell whether their next `carrick init` is
 * a wait (carrick#1373).
 */
export function hostedReport(outcome: HostedDownload, seconds: number | null = null): StepReport {
  const plural = (count: number): string => `${count} service${count === 1 ? "" : "s"}`;
  // The total and the time, on the line that stays once the ticking one is
  // gone: a wait of minutes that ends on a line with no duration leaves the
  // reader to guess whether that was normal (carrick#1365).
  const took = seconds === null ? "" : ` in ${elapsed(seconds)}`;
  switch (outcome.kind) {
    case "downloaded":
      return {
        kind: "done",
        text: outcome.reread
          ? `Hosted index for ${plural(outcome.services)} read into .carrick/${took}`
          : `.carrick/ already holds the hosted index for ${plural(outcome.services)}: nothing was re-read`,
      };
    case "version_mismatch":
      return {
        kind: "warn",
        text: "Hosted index is older than this CLI: run `carrick index --detach` once from main",
      };
    case "local_only":
      return {
        kind: "warn",
        text: `.carrick/ ${outcome.reread ? "holds" : "already holds"} ${
          plural(outcome.services)
        } as this machine read them; ${stateClause(outcome.state)}`,
      };
    case "scanning":
      return {
        kind: "warn",
        text: "A scan is building this index here, so nothing was re-read. Run `carrick status` to see how far it has got",
      };
    case "failed":
      return { kind: "refuse", text: `Hosted index could not be read into .carrick/: ${outcome.problem}` };
  }
}
