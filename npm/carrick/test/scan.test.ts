// What a first run sees (carrick#1315).
//
// The interactive rendering is driven through a stream the test holds, which
// is the only way to read it back: a terminal's own bytes are not capturable,
// and the plain rendering is a different renderer with different lines
// (carrick#1032). The end-to-end pair spawn the fake binary, which writes the
// stream a real build writes — markers, the binary's own plain lines, and the
// map — so what the renderer drops is asserted against the thing it drops.

import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { PassThrough } from "node:stream";
import { fileURLToPath } from "node:url";
import { interactiveOutput, plainOutput } from "../src/init/output.ts";
import {
  elapsed,
  isRendered,
  parseMarker,
  renderScan,
  ScanRender,
  summaryLine,
  timingLine,
} from "../src/scan.ts";

const fakeScan = fileURLToPath(new URL("./fake-scan.mjs", import.meta.url));

/** The stream a build of one service writes, as the fake replays it. */
const BUILD = [
  'Carrick run starting run_id="8ab3" scanner_version="0.3.81" ci=<unset>',
  "carrick: indexing 1 repos (carrick.json): /code/api",
  '@carrick-phase {"label":"indexing api","state":"started"}',
  '@carrick-progress {"service":"api","service_index":1,"service_total":1,"phase":"files","done":95,"total":95}',
  "✓ indexed api",
  '@carrick-phase {"label":"indexed api","state":"done"}',
  '@carrick-phase {"label":"joining the workspace","state":"started"}',
  '@carrick-phase {"label":"joined the workspace","state":"done"}',
  '@carrick-summary {"services":[{"name":"pan-api","routes":111,"calls":10,"functions":363,"types":175,"routes_without_response_type":46}],"elapsed_secs":169.4,"timing":{"files":1204,"services":5,"local_secs":61,"model_secs":96.4,"upload_secs":12}}',
];

/** What the build says before it starts, read from the last one (carrick#1452). */
const TREE_LINE = "Reading the tree: 1204 files across 5 services; last time 2m13s.";

/**
 * The interactive rendering as a reader sees it: colour and cursor control
 * removed, so an assertion is about the line and not about clack's escapes.
 */
function captured(): { stream: PassThrough; text: () => string } {
  const stream = new PassThrough();
  const chunks: Buffer[] = [];
  stream.on("data", (chunk: Buffer) => chunks.push(chunk));
  return {
    stream,
    text: () =>
      Buffer.concat(chunks)
        .toString("utf8")
        // eslint-disable-next-line no-control-regex
        .replace(/\u001b\[[0-9;?]*[a-zA-Z]/g, ""),
  };
}

test("a build renders as an intro, one step per phase and the counts", async () => {
  const { stream, text } = captured();
  const render = new ScanRender(interactiveOutput(stream), "0.3.81");
  // Said before the first phase, which is what makes it the line a reader
  // meets before the wait rather than one more thing after it.
  render.stdout(TREE_LINE);
  for (const line of BUILD) render.stderr(line);
  assert.equal(await render.finish(false), true);

  const drawn = text();
  // The block, in the shape the ticket accepts: an intro carrying the
  // version, what the wait is expected to be, one step per phase, the counts,
  // what the wait was, and the one next step.
  assert.deepEqual(
    drawn
      .split("\n")
      .map((line) => line.trimEnd())
      .filter((line) => line.length > 0 && line !== "│"),
    [
      "┌  carrick 0.3.81",
      // A line with no state of its own, which is what both of these are:
      // neither is a step that succeeded or failed.
      `│  ${TREE_LINE}`,
      `◇  indexing api  95 files, ${elapsed(0)}`,
      "◇  111 routes · 363 functions · 175 types · 10 external calls · 46 routes without a response type",
      "│  local read 1m1s · model analysis 1m36s · upload 12.0s",
      "└  Your agents can query it now. `carrick index --verbose` shows the full report.",
    ],
  );
  // And nothing of the three faults the ticket names.
  assert.ok(!drawn.includes("Carrick run starting"), drawn);
  assert.ok(!drawn.includes("carrick: indexing 1 repos"), drawn);
  assert.ok(!drawn.includes("boundary ("), drawn);
  // The join is not a line of its own: the counts are its outcome.
  assert.ok(!drawn.includes("joined the workspace"), drawn);
});

test("the plain rendering is the same lines, with no banner and no map", async (t) => {
  const written: string[] = [];
  const render = new ScanRender(
    plainOutput((text) => written.push(text)),
    "0.3.81",
  );
  for (const line of BUILD) render.stderr(line);
  // The map arrives on stdout, after the first phase.
  render.stdout("boundary (pan-api): candidates: from the hosted index at 1bf3de5");
  assert.equal(await render.finish(false), true);

  const drawn = written.join("");
  assert.match(drawn, /^carrick 0\.3\.81\n/);
  assert.match(drawn, /◇ indexing api {2}95 files/);
  assert.match(drawn, /◇ 111 routes · 363 functions · 175 types · 10 external calls/);
  assert.match(drawn, /Your agents can query it now\./);
  assert.ok(!drawn.includes("Carrick run starting"), drawn);
  assert.ok(!drawn.includes("boundary ("), drawn);
  t.diagnostic(drawn.trim());
});

test("a build that hands its analysis over closes on how to collect it", async () => {
  const { stream, text } = captured();
  const render = new ScanRender(interactiveOutput(stream), "0.3.81");
  render.stderr('@carrick-phase {"label":"indexing api","state":"started"}');
  render.stderr('@carrick-phase {"label":"indexed api","state":"done"}');
  render.stderr(
    '@carrick-summary {"services":[],"elapsed_secs":41.2,"next":["Carrick Cloud is analysing acme/api (95 file(s)).","`carrick resume` builds the index when it is done."]}',
  );
  assert.equal(await render.finish(false), true);

  const drawn = text();
  assert.match(drawn, /Carrick Cloud is analysing acme\/api/);
  assert.match(drawn, /carrick resume/);
  // No counts line: a dispatched build indexed nothing, and a row of zeroes
  // would read as a build that found nothing.
  assert.ok(!drawn.includes("0 routes"), drawn);
  // The phase keeps its own line, because the summary had nothing to put there.
  assert.match(drawn, /indexing api/);
});

test("what a command says before its first phase is its answer, and is shown", async () => {
  const { stream, text } = captured();
  const render = new ScanRender(interactiveOutput(stream), "0.3.81");
  render.stdout("Collected the analysis of acme/api (95 file(s)).");
  for (const line of BUILD) render.stderr(line);
  await render.finish(false);
  assert.match(text(), /Collected the analysis of acme\/api/);
});

test("a run that renders nothing writes its output through", async () => {
  const outcome = await renderScan({
    binary: process.execPath,
    args: [fakeScan],
    env: { ...process.env, CARRICK_FAKE_SCAN: "silent" },
    version: "0.3.81",
    output: plainOutput(() => {
      throw new Error("nothing should have been drawn");
    }),
  });
  assert.equal(outcome.code, 0);
});

test("the binary's exit code is this process's, and a failure prints what it said", async () => {
  const written: string[] = [];
  const outcome = await renderScan({
    binary: process.execPath,
    args: [fakeScan],
    env: { ...process.env, CARRICK_FAKE_EXIT: "3" },
    version: "0.3.81",
    output: plainOutput((text) => written.push(text)),
  });
  assert.equal(outcome.code, 3);
  // Drawn as far as it got, and the raw output kept for the reader: a failure
  // is the one time the diagnostics are the answer.
  assert.match(written.join(""), /carrick 0\.3\.81/);
});

test("the markers are asked for, so a run nobody renders is unchanged", async () => {
  // The renderer sets the flag on the child; the binary writes markers only
  // when it is set, which is why a CI scan's log is not full of them.
  const seen: string[] = [];
  await renderScan({
    binary: process.execPath,
    args: [
      "-e",
      "process.stdout.write(String(process.env.CARRICK_PROGRESS ?? 'unset'))",
    ],
    env: { ...process.env },
    version: "0.3.81",
    output: plainOutput((text) => seen.push(text)),
  });
  assert.ok(!seen.join("").includes("unset"), seen.join(""));
});

test("verbose, detach and help are the binary's own output, not a rendering", () => {
  assert.equal(isRendered("index", []), true);
  assert.equal(isRendered("resume", ["--workspace", "/w"]), true);
  assert.equal(isRendered("refresh", []), true);
  assert.equal(isRendered("index", ["--verbose"]), false);
  assert.equal(isRendered("index", ["-v"]), false);
  assert.equal(isRendered("index", ["--detach"]), false);
  assert.equal(isRendered("index", ["--help"]), false);
  assert.equal(isRendered("status", []), false);
  assert.equal(isRendered("check", ["a.ts", "--json"]), false);
  assert.equal(isRendered(undefined, []), false);
});

test("a marker is read by its prefix, and nothing else is read as one", () => {
  assert.equal(parseMarker("carrick: indexing 1 repos"), null);
  assert.equal(parseMarker("@carrick-phase not json"), null);
  assert.equal(parseMarker('@carrick-phase {"state":"done"}'), null);
  assert.equal(parseMarker("@carrick-summary []"), null);
  assert.deepEqual(parseMarker('@carrick-phase {"label":"indexing api","state":"started"}'), {
    kind: "phase",
    phase: { label: "indexing api", state: "started" },
  });
  assert.deepEqual(parseMarker('@carrick-notice {"text":"model busy"}'), {
    kind: "notice",
    text: "model busy",
  });
});

test("counts and durations read the way the line says them", () => {
  assert.equal(elapsed(169.4), "2m49s");
  assert.equal(elapsed(32.74), "32.7s");
  assert.equal(elapsed(60), "1m0s");
  assert.equal(
    summaryLine({
      services: [
        { name: "a", routes: 1, calls: 1, functions: 3, types: 5, routes_without_response_type: 1 },
        {
          name: "b",
          routes: 110,
          calls: 9,
          functions: 360,
          types: 170,
          routes_without_response_type: 45,
        },
      ],
      elapsed_secs: 1,
    }),
    "111 routes · 363 functions · 175 types · 10 external calls · 46 routes without a response type",
  );
  // Nothing to do about, so nothing said. Calls stay on the line at zero:
  // "no external calls" is a fact about a service, not a shortfall in it.
  assert.equal(
    summaryLine({
      services: [
        { name: "a", routes: 1, calls: 0, functions: 2, types: 1, routes_without_response_type: 0 },
      ],
      elapsed_secs: 1,
    }),
    "1 route · 2 functions · 1 type · 0 external calls",
  );
  // A binary older than carrick#1321 states neither new count, and the line
  // drops both rather than printing a zero nobody measured.
  assert.equal(
    summaryLine({
      services: [{ name: "a", routes: 4, calls: 2, routes_without_response_type: 0 }],
      elapsed_secs: 1,
    }),
    "4 routes · 2 external calls",
  );
  // What the wait was made of, in the same three parts the next run's opening
  // line quotes (carrick#1452). Time only: what a run costs is never here.
  assert.equal(
    timingLine({ files: 1204, services: 5, local_secs: 61, model_secs: 96.4, upload_secs: 12 }),
    "local read 1m1s · model analysis 1m36s · upload 12.0s",
  );
});

test("the whole stream, end to end, through the spawned binary", async (t) => {
  const written: string[] = [];
  const outcome = await renderScan({
    binary: process.execPath,
    args: [fakeScan],
    env: { ...process.env },
    version: "0.3.81",
    output: plainOutput((text) => written.push(text)),
  });
  assert.equal(outcome.code, 0);
  const drawn = written.join("");
  assert.match(drawn, /◇ indexing api {2}95 files/);
  assert.match(drawn, /◇ 111 routes · 363 functions · 175 types · 10 external calls/);
  // The two lines the wait is bracketed by, both through the spawned binary:
  // what the last read took, and what this one took (carrick#1452).
  assert.ok(drawn.indexOf(TREE_LINE) < drawn.indexOf("indexing api"), drawn);
  assert.match(drawn, /local read 1m1s · model analysis 1m36s · upload 12\.0s/);
  assert.ok(!drawn.includes("Carrick run starting"), drawn);
  assert.ok(!drawn.includes("boundary ("), drawn);
  assert.ok(!drawn.includes("SDK call(s) that produced no edge"), drawn);
  assert.ok(!drawn.includes("@carrick-"), drawn);
  t.diagnostic(drawn.trim());
});

test("the fake's own path is what a test spawns", () => {
  assert.equal(path.basename(fakeScan), "fake-scan.mjs");
});
