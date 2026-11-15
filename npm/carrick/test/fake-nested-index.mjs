#!/usr/bin/env node
// A stand-in for `carrick check` that picks its `.carrick/` the way the binary
// does: an explicit `--workspace` first, else the nearest one above the file.
// It answers CARRICK_FAKE_FIXTURE with `index_commit` read from that folder's
// `index.json`, so a test can tell which of two nested folders answered.
import fs from "node:fs";
import path from "node:path";

const argv = process.argv.slice(2);
const at = argv.indexOf("--workspace");
const files = argv.filter((a, i) => i > 0 && !a.startsWith("--") && argv[i - 1] !== "--workspace");
let dir = at >= 0 ? argv[at + 1] : path.dirname(path.resolve(files[0]));
while (at < 0 && !fs.existsSync(path.join(dir, ".carrick", "index.json")) && path.dirname(dir) !== dir) {
  dir = path.dirname(dir);
}
const commit = JSON.parse(fs.readFileSync(path.join(dir, ".carrick", "index.json"), "utf8")).commit;
const answer = { ...JSON.parse(fs.readFileSync(process.env.CARRICK_FAKE_FIXTURE, "utf8")), index_commit: commit };
process.stdout.write(argv.includes("--each") ? JSON.stringify(files.map(() => answer)) : JSON.stringify(answer));
