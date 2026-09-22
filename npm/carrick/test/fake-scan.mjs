#!/usr/bin/env node
// A stand-in for the binary's `index`, for the renderer's tests.
//
// What it writes is the stream a real build writes, captured from
// `carrick refresh` with CARRICK_PROGRESS set: the markers on stderr, the
// binary's own plain lines beside them, and the map on stdout. The renderer
// has to keep the first and drop the second and third, so the fake carries
// all three rather than the markers alone.
//
// Env it honours:
//   CARRICK_FAKE_SCAN   which stream to write:
//                       "index"    a two-service build that finishes (default)
//                       "dispatch" a build that handed its analysis over
//                       "silent"   a command that states no marker at all
//   CARRICK_FAKE_EXIT   exit code, after writing the stream

import process from "node:process";

const out = (line) => process.stdout.write(`${line}\n`);
const err = (line) => process.stderr.write(`${line}\n`);

const mode = process.env.CARRICK_FAKE_SCAN ?? "index";

if (mode === "silent") {
  out("Nothing from this workspace is being analysed. `carrick index` builds the index here.");
  process.exit(Number(process.env.CARRICK_FAKE_EXIT ?? 0));
}

// The banner the binary logs at `--verbose` and the prose it prints either
// way. Both are here so a test can assert they are not rendered.
err(
  'Carrick run starting run_id="8ab3" scanner_version="0.3.81" ci=<unset> github_event=<unset>',
);
err("carrick: indexing 1 repos (carrick.json): /code/api");
err(
  "carrick: this scan asks Carrick Cloud to classify what the deterministic passes could not.",
);
// The last thing said before the wait, on stdout because a build's stderr is
// kept for a failure and shown for nothing else (carrick#1452).
out("Reading the tree: 1204 files across 5 services; last time 2m13s.");

if (mode === "dispatch") {
  err('@carrick-phase {"label":"indexing api","state":"started"}');
  err('@carrick-progress {"service":"api","service_index":1,"service_total":1,"phase":"files","done":95,"total":95}');
  err("✓ indexed api");
  err('@carrick-phase {"label":"indexed api","state":"done"}');
  out("Carrick Cloud is analysing acme/api (95 file(s)).");
  err(
    '@carrick-summary {"services":[],"elapsed_secs":41.2,"next":["Carrick Cloud is analysing acme/api (95 file(s)).","`carrick resume` builds the index when it is done."]}',
  );
  process.exit(Number(process.env.CARRICK_FAKE_EXIT ?? 0));
}

err('@carrick-phase {"label":"indexing api","state":"started"}');
err('@carrick-progress {"service":"api","service_index":1,"service_total":1,"phase":"files","done":40,"total":95}');
err('@carrick-notice {"text":"model busy: slowing analyze-file to 4 requests at a time"}');
err('@carrick-progress {"service":"api","service_index":1,"service_total":1,"phase":"files","done":95,"total":95}');
err("✓ indexed api");
err('@carrick-phase {"label":"indexed api","state":"done"}');
err('@carrick-phase {"label":"joining the workspace","state":"started"}');
err("✓ joined the workspace");
err('@carrick-phase {"label":"joined the workspace","state":"done"}');
err(
  '@carrick-summary {"services":[{"name":"pan-api","routes":111,"calls":10,"functions":363,"types":175,"routes_without_response_type":46}],"elapsed_secs":169.4,"timing":{"files":1204,"services":5,"local_secs":61,"model_secs":96.4,"upload_secs":12}}',
);

out("");
out("indexed 1 repo(s) in 169.4s at 2026-09-17T13:31:59Z");
out("  local read 1m1s · model analysis 1m36s · upload 12.0s");
out("  pan-api                       111 route(s)    10 call(s)  1bf3de5");
out("  0 counterpart link(s) across the workspace");
out("");
out(
  "boundary (pan-api): candidates: from the hosted index at 1bf3de5; 0 file(s) changed since then hold facts only.",
);
out("    1357 SDK call(s) that produced no edge (e.g. a framework package x519: no_sdk_repo_in_project)");

process.exit(Number(process.env.CARRICK_FAKE_EXIT ?? 0));
