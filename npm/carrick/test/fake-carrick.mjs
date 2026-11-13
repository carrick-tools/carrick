#!/usr/bin/env node
// A stand-in for the `carrick` binary, for tests and for the selftest.
//
// The real CLI lands with carrick#708. Until it does, every test here drives
// the plugin against fixture payloads that match `carrick.check/0`, and this
// script is what CARRICK_BIN points at.
//
// Env it honours:
//   CARRICK_FAKE_FIXTURE  path to the JSON to print (default check-mismatch.json)
//   CARRICK_FAKE_FIXTURE_MAP
//                         path to a JSON object mapping a checked file, as the
//                         caller passed it, to the fixture to answer it with.
//                         A file it does not name falls back to the above, so a
//                         test that needs the two sides of one contract to
//                         answer differently says only what differs
//   CARRICK_FAKE_DELAY_MS milliseconds to wait before printing
//   CARRICK_FAKE_PROBE_FIXTURE
//                         path to the JSON that answers a call carrying
//                         `--no-rescan` (the post-edit probe); every other
//                         call is answered as above
//   CARRICK_FAKE_FULL_DELAY_MS
//                         milliseconds to wait before printing, on calls
//                         WITHOUT `--no-rescan` only (the full re-check)
//   CARRICK_FAKE_PROBE_DELAY_MS
//                         milliseconds to wait before printing, on calls
//                         WITH `--no-rescan` only (the post-edit probe)
//   CARRICK_FAKE_PID_LOG  file to append this process's pid to, and, with it,
//                         the process ignores SIGTERM (so only SIGKILL ends a
//                         call that is past its limit)
//   CARRICK_FAKE_DONE_LOG file to append a line to when a call reaches its
//                         answer (after any delay), so a test can read that a
//                         call was still pending when its caller returned
//   CARRICK_FAKE_EXIT     exit code to use, with nothing on stdout
//   CARRICK_FAKE_ARGV_LOG file to append the argv and cwd of every call to
//   CARRICK_FAKE_INVOKER_LOG
//                         file to append the CARRICK_INVOKER of every call to
//                         ("" when unset), to read what a hook marked itself as
//   CARRICK_FAKE_ENV_LOG  file to append {budget, timeout} (CARRICK_RECHECK_BUDGET_MS
//                         and CARRICK_TIMEOUT_MS) of every call to
//   CARRICK_FAKE_REBASE   "1" rewrites the fixtures' `/workspace` prefix to the
//                         working directory, so the absolute `repo` paths a
//                         payload carries point at the throwaway workspace an
//                         integration test just built

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);

const argvLog = process.env.CARRICK_FAKE_ARGV_LOG;
if (argvLog) {
  fs.appendFileSync(argvLog, `${JSON.stringify({ argv, cwd: process.cwd() })}\n`);
}

if (process.env.CARRICK_FAKE_PID_LOG) {
  fs.appendFileSync(process.env.CARRICK_FAKE_PID_LOG, `${process.pid}\n`);
  process.on("SIGTERM", () => {});
}

const invokerLog = process.env.CARRICK_FAKE_INVOKER_LOG;
if (invokerLog) {
  fs.appendFileSync(invokerLog, `${process.env.CARRICK_INVOKER ?? ""}\n`);
}

const envLog = process.env.CARRICK_FAKE_ENV_LOG;
if (envLog) {
  fs.appendFileSync(
    envLog,
    `${JSON.stringify({
      budget: process.env.CARRICK_RECHECK_BUDGET_MS ?? null,
      timeout: process.env.CARRICK_TIMEOUT_MS ?? null,
    })}\n`,
  );
}

const exitCode = process.env.CARRICK_FAKE_EXIT ? Number(process.env.CARRICK_FAKE_EXIT) : 0;
if (exitCode !== 0) {
  process.stderr.write("fake carrick: asked to fail\n");
  process.exit(exitCode);
}

// `check --changed --json`: prints CARRICK_FAKE_CHANGED_FIXTURE (default
// check-changed-clean.json), exits CARRICK_FAKE_CHANGED_EXIT (default 0), and
// leaves the marker the binary writes (`record_check_marker`, changed.rs).
if (argv.includes("--changed")) {
  const fixture = process.env.CARRICK_FAKE_CHANGED_FIXTURE
    ? path.resolve(process.env.CARRICK_FAKE_CHANGED_FIXTURE)
    : path.join(here, "fixtures", "check-changed-clean.json");
  const code = process.env.CARRICK_FAKE_CHANGED_EXIT ? Number(process.env.CARRICK_FAKE_CHANGED_EXIT) : 0;
  const started = Date.now();
  fs.mkdirSync(path.join(process.cwd(), ".carrick"), { recursive: true });
  fs.writeFileSync(
    path.join(process.cwd(), ".carrick", "last-check-changed.json"),
    JSON.stringify({ started_at_ms: started, finished_at_ms: Date.now(), exit: code }),
  );
  process.stdout.write(fs.readFileSync(fixture, "utf8"));
  process.exit(code);
}

const probe = argv.includes("--no-rescan");
const fallbackFixture =
  probe && process.env.CARRICK_FAKE_PROBE_FIXTURE
    ? path.resolve(process.env.CARRICK_FAKE_PROBE_FIXTURE)
    : process.env.CARRICK_FAKE_FIXTURE
      ? path.resolve(process.env.CARRICK_FAKE_FIXTURE)
      : path.join(here, "fixtures", "check-mismatch.json");

/** The files this call is about: `check <file> --json`, so the non-flags. */
const files = argv.slice(1).filter((value) => !value.startsWith("--"));
const map = process.env.CARRICK_FAKE_FIXTURE_MAP
  ? JSON.parse(fs.readFileSync(process.env.CARRICK_FAKE_FIXTURE_MAP, "utf8"))
  : {};
/** The fixture body that answers one file. */
const answer = (file) => {
  const fixture = file && map[file] ? path.resolve(map[file]) : fallbackFixture;
  let body = fs.readFileSync(fixture, "utf8");
  if (process.env.CARRICK_FAKE_REBASE === "1") {
    body = body.split('"/workspace').join(`"${process.cwd()}`);
  }
  return body;
};

const delay =
  (process.env.CARRICK_FAKE_DELAY_MS ? Number(process.env.CARRICK_FAKE_DELAY_MS) : 0) +
  (!probe && process.env.CARRICK_FAKE_FULL_DELAY_MS ? Number(process.env.CARRICK_FAKE_FULL_DELAY_MS) : 0) +
  (probe && process.env.CARRICK_FAKE_PROBE_DELAY_MS ? Number(process.env.CARRICK_FAKE_PROBE_DELAY_MS) : 0);
const print = () => {
  if (process.env.CARRICK_FAKE_DONE_LOG) {
    fs.appendFileSync(process.env.CARRICK_FAKE_DONE_LOG, `${probe ? "probe" : "full"}\n`);
  }
  // `check --each a b --json`: one array, one answer per file, in order.
  const body = argv.includes("--each")
    ? `[${files.map(answer).join(",")}]`
    : answer(files[0]);
  process.stdout.write(body);
  process.exit(0);
};
if (delay > 0) setTimeout(print, delay);
else print();
