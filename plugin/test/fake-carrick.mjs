#!/usr/bin/env node
// A stand-in for the `carrick` binary, for tests and for the selftest.
//
// The real CLI lands with carrick#708. Until it does, every test here drives
// the plugin against fixture payloads that match `carrick.check/0`, and this
// script is what CARRICK_BIN points at.
//
// Env it honours:
//   CARRICK_FAKE_FIXTURE  path to the JSON to print (default check-mismatch.json)
//   CARRICK_FAKE_DELAY_MS milliseconds to wait before printing
//   CARRICK_FAKE_EXIT     exit code to use, with nothing on stdout
//   CARRICK_FAKE_ARGV_LOG file to append the argv and cwd of every call to

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);

const argvLog = process.env.CARRICK_FAKE_ARGV_LOG;
if (argvLog) {
  fs.appendFileSync(argvLog, `${JSON.stringify({ argv, cwd: process.cwd() })}\n`);
}

const exitCode = process.env.CARRICK_FAKE_EXIT ? Number(process.env.CARRICK_FAKE_EXIT) : 0;
if (exitCode !== 0) {
  process.stderr.write("fake carrick: asked to fail\n");
  process.exit(exitCode);
}

const fixture = process.env.CARRICK_FAKE_FIXTURE
  ? path.resolve(process.env.CARRICK_FAKE_FIXTURE)
  : path.join(here, "fixtures", "check-mismatch.json");

const delay = process.env.CARRICK_FAKE_DELAY_MS ? Number(process.env.CARRICK_FAKE_DELAY_MS) : 0;
const print = () => {
  process.stdout.write(fs.readFileSync(fixture, "utf8"));
  process.exit(0);
};
if (delay > 0) setTimeout(print, delay);
else print();
