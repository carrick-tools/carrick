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
// carrick#985 will turn it into a fetch where the local blob is already the
// fresher one; the command this calls does not change when it does.
//
// What it reports is read back from `carrick status`, not assumed from an
// exit code: a refresh that scanned fine can still have been refused the
// hosted rows, and the sentence a reader needs is different in each case
// (carrick#1012).

import { spawn } from "node:child_process";
import { parseStatusResult } from "../contract.ts";
import { nativeEnv, resolveNativeBinary } from "../native.ts";
import { elapsed, parseMarker } from "../scan.ts";
import type { StepProgress, StepReport } from "./output.ts";

/**
 * One run of the scanner binary, injected so tests spawn nothing.
 *
 * `watch` asks for the run's progress markers: the download is the one call
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
  | { kind: "downloaded"; services: number }
  /** The hosted blob predates this CLI, so its rows were not replayed. */
  | { kind: "version_mismatch"; services: number }
  /** There is a local index, without the hosted rows: `state` says why. */
  | { kind: "local_only"; services: number; state: string }
  /** There is no local index at all, and `problem` is why. */
  | { kind: "failed"; problem: string };

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
        for (const line of lines) report(line);
      }
      // Quiet under a spinner: a spinner owns its line and rewrites it, and a
      // scanner line landing in the middle of that is a corrupted terminal.
      // Where there is no spinner — a pipe, CI, an agent shell — the progress
      // is the only sign a minutes-long read is alive (carrick#1021). The
      // marker lines are the spinner's own input and are never written through.
      if (!quiet && !args.includes("--json")) process.stderr.write(withoutMarkers(chunk));
    });
    child.on("error", (error) => resolve({ status: 1, stdout: "", stderr: error.message }));
    child.on("close", (code) => resolve({ status: code, stdout, stderr }));
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
export function downloadProgress(say: StepProgress, now: () => number = Date.now): (line: string) => void {
  const startedAt = now();
  let services = "";
  let bytes = "";
  return (line: string): void => {
    const marker = parseMarker(line);
    if (marker === null) return;
    if (marker.kind === "progress") {
      const update = marker.update;
      services = `${update.service_index} of ${update.service_total} services`;
      say([DOWNLOAD_LABEL, `${services}${bytes}, ${elapsed((now() - startedAt) / 1000)}`].join(": "));
      return;
    }
    // The hosted half says how much it read; the scanner states it as a notice
    // because it is one fact, once, not a count that ticks.
    if (marker.kind === "notice" && marker.text.startsWith(HOSTED_BYTES)) {
      bytes = `, ${marker.text.slice(HOSTED_BYTES.length)}`;
      say([DOWNLOAD_LABEL, `${services || "reading"}${bytes}, ${elapsed((now() - startedAt) / 1000)}`].join(": "));
    }
  };
}

/** The label the step carries, and the stem of every line it writes. */
export const DOWNLOAD_LABEL = "Downloading your index";

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
 * Populate `.carrick/` from the hosted index, and say what arrived.
 *
 * `workspace` is the derived workspace root — the directory `carrick init`
 * wrote the proposal into — so the index this builds is the one every later
 * read of that workspace finds.
 */
export async function downloadHostedIndex(
  workspace: string,
  run: NativeRun = nativeRunner(false),
): Promise<HostedDownload> {
  const refreshed = await run(["refresh", "--workspace", workspace], true);
  if (refreshed.status !== 0) {
    return { kind: "failed", problem: firstLine(refreshed.stderr, "the scanner gave no reason") };
  }
  const answer = await run(["status", "--workspace", workspace, "--json"]);
  const status = answer.status === 0 ? parseStatusResult(answer.stdout) : null;
  if (!status || status.services.length === 0) {
    return {
      kind: "failed",
      problem: firstLine(
        status?.message ?? answer.stderr,
        "the index it wrote holds no service this build can read",
      ),
    };
  }
  const services = status.services.length;
  // Stated before the count, because it is the one state with an instruction
  // attached: a blob older than this CLI is not replayed at all (carrick#1012).
  if (status.services.some((service) => service.hosted_state === "version_mismatch")) {
    return { kind: "version_mismatch", services };
  }
  const enriched = status.services.filter((service) => service.hosted_state === "enriched");
  if (enriched.length === 0) {
    const state = status.services.find((service) => service.hosted_state)?.hosted_state ?? "unknown";
    return { kind: "local_only", services, state };
  }
  return { kind: "downloaded", services: enriched.length };
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
 */
export function hostedReport(outcome: HostedDownload, seconds: number | null = null): StepReport {
  const plural = (count: number): string => `${count} service${count === 1 ? "" : "s"}`;
  // The total and the time, on the line that stays once the ticking one is
  // gone: a wait of minutes that ends on a line with no duration leaves the
  // reader to guess whether that was normal (carrick#1365).
  const took = seconds === null ? "" : ` in ${elapsed(seconds)}`;
  switch (outcome.kind) {
    case "downloaded":
      return { kind: "done", text: `Hosted index for ${plural(outcome.services)} downloaded into .carrick/${took}` };
    case "version_mismatch":
      return {
        kind: "warn",
        text: "Hosted index is older than this CLI: run `carrick index --detach` once from main",
      };
    case "local_only":
      return {
        kind: "warn",
        text: `.carrick/ holds ${plural(outcome.services)} as this machine read them; ${stateClause(outcome.state)}`,
      };
    case "failed":
      return { kind: "refuse", text: `Hosted index could not be read into .carrick/: ${outcome.problem}` };
  }
}
