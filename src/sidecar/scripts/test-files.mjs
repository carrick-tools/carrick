// The one list of sidecar test files: `npm test` and CI's sidecar shards both
// read it (carrick#4310). A shell glob in package.json missed every top-level
// file, so the list lives here, expanded by node, not by sh.
//
//   node scripts/test-files.mjs [all|heavy|rest-1|rest-2|rest-3]   print files
//   node scripts/test-files.mjs run [shard] [-- node --test args]  run them
import { readdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
// Real package managers, Deno and tsc over fixture projects: slow, own shard.
const HEAVY = /^(deno-project|check-v2-corpus|capture-v2-installed-pin|sidecar\.test\.js)/;

export function allTestFiles(dir = join(root, "dist", "test")) {
  return readdirSync(dir)
    .filter((f) => f.endsWith(".test.js"))
    .sort()
    .map((f) => `dist/test/${f}`);
}

export function shardFiles(shard, files = allTestFiles()) {
  const name = (f) => f.slice("dist/test/".length);
  if (shard === "all") return files;
  if (shard === "heavy") return files.filter((f) => HEAVY.test(name(f)));
  const m = /^rest-([123])$/.exec(shard);
  if (!m) throw new Error(`unknown shard: ${shard}`);
  const n = Number(m[1]);
  // Every third file by position, so a new file lands in a shard with no edit.
  return files.filter((f) => !HEAVY.test(name(f))).filter((_, i) => (i + 1) % 3 === n % 3);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const run = args[0] === "run";
  if (run) args.shift();
  const sep = args.indexOf("--");
  const extra = sep >= 0 ? args.slice(sep + 1) : [];
  const shard = (sep >= 0 ? args.slice(0, sep) : args)[0] ?? "all";
  const files = shardFiles(shard);
  if (!run) {
    console.log(files.join("\n"));
  } else {
    console.error(`${shard}: ${files.length} files`);
    const r = spawnSync(process.execPath, ["--test", ...extra, ...files], { cwd: root, stdio: "inherit" });
    process.exit(r.status ?? 1);
  }
}
