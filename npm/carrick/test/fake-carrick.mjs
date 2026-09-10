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
//   CARRICK_FAKE_EXIT     exit code to use, with nothing on stdout
//   CARRICK_FAKE_ARGV_LOG file to append the argv and cwd of every call to
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

const exitCode = process.env.CARRICK_FAKE_EXIT ? Number(process.env.CARRICK_FAKE_EXIT) : 0;
if (exitCode !== 0) {
  process.stderr.write("fake carrick: asked to fail\n");
  process.exit(exitCode);
}

const fallbackFixture = process.env.CARRICK_FAKE_FIXTURE
  ? path.resolve(process.env.CARRICK_FAKE_FIXTURE)
  : path.join(here, "fixtures", "check-mismatch.json");

/** The file this call is about: `check <file> --json`, so the first non-flag. */
const asked = argv.slice(1).find((value) => !value.startsWith("--"));
let fixture = fallbackFixture;
if (process.env.CARRICK_FAKE_FIXTURE_MAP && asked) {
  const map = JSON.parse(fs.readFileSync(process.env.CARRICK_FAKE_FIXTURE_MAP, "utf8"));
  if (map[asked]) fixture = path.resolve(map[asked]);
}

const delay = process.env.CARRICK_FAKE_DELAY_MS ? Number(process.env.CARRICK_FAKE_DELAY_MS) : 0;
const print = () => {
  let body = fs.readFileSync(fixture, "utf8");
  if (process.env.CARRICK_FAKE_REBASE === "1") {
    body = body.split('"/workspace').join(`"${process.cwd()}`);
  }
  process.stdout.write(body);
  process.exit(0);
};
if (delay > 0) setTimeout(print, delay);
else print();
