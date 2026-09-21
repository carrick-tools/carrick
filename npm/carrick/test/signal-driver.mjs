#!/usr/bin/env node
// `carrick index` from the outside, for the test that signals it
// (carrick#1391).
//
// A separate process because the defect is what THIS process does when it is
// signalled: in-process the harness's own listeners are in the way. It runs
// the fake scan through the real renderer and exits with what that answers,
// which is what `bin/carrick.mjs` does.
//
// Argv: <fake scan path> <file the fake records its signal in>

import process from "node:process";
import { renderScan } from "../dist/scan.js";

const [fake, record] = process.argv.slice(2);

const outcome = await renderScan({
  binary: process.execPath,
  args: [fake],
  env: { ...process.env, CARRICK_FAKE_SIGNAL_FILE: record },
  version: "0.0.0-test",
});
process.stderr.write(`driver: child code=${outcome.code} signal=${outcome.signal}\n`);
process.exit(outcome.code);
