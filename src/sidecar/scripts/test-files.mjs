// The one list of sidecar test files: `npm test` and CI's sidecar shards both
// read it (carrick#4310). A shell glob in package.json missed every top-level
// file, so the list lives here, expanded by node, not by sh.
//
//   node scripts/test-files.mjs [all|heavy|rest-1|rest-2|rest-3]   print files
//   node scripts/test-files.mjs run [shard] [-- node --test args]  run them
//
// `run` gives every file its own `node --test` process and a time limit, so a
// file that never finishes is killed and named instead of stalling the job
// silently (carrick#4752: one CI shard printed nothing for 42 minutes).
import { readdirSync } from "node:fs";
import { spawn } from "node:child_process";
import { availableParallelism } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
// Real package managers, Deno and tsc over fixture projects: slow, own shard.
const HEAVY = /^(deno-project|check-v2-corpus|capture-v2-installed-pin|sidecar\.test\.js)/;

// Per-file limit. The slowest rest-shard file takes about 12 s locally and the
// slowest heavy file (real package managers, Deno, tsc) a few minutes on CI,
// so 10 minutes is wide headroom: only a hang reaches it. Tests override it
// through SIDECAR_TEST_FILE_TIMEOUT_MS.
export const FILE_TIMEOUT_MS = Number(process.env.SIDECAR_TEST_FILE_TIMEOUT_MS) || 10 * 60 * 1000;

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

// Runs each file in its own process, up to `jobs` at once (node's own default
// for several files), and prints a file's output when it ends. Resolves to the
// files that failed or timed out.
export async function runFiles(files, { extra = [], timeoutMs = FILE_TIMEOUT_MS, cwd = root, jobs = Math.max(1, availableParallelism() - 1) } = {}) {
  const failed = [];
  const queue = [...files];
  const runOne = (file) =>
    new Promise((resolve) => {
      const out = [];
      // Own process group, so the kill reaches grandchildren holding the pipe.
      // NODE_TEST_CONTEXT is dropped so a run started from inside a test (the
      // runner's own test) is a real run, not a reporter-less child.
      const { NODE_TEST_CONTEXT, ...env } = process.env;
      const child = spawn(process.execPath, ["--test", ...extra, file], { cwd, env, detached: true, stdio: ["ignore", "pipe", "pipe"] });
      child.stdout.on("data", (d) => out.push(d));
      child.stderr.on("data", (d) => out.push(d));
      let timedOut = false;
      const timer = setTimeout(() => {
        timedOut = true;
        try { process.kill(-child.pid, "SIGKILL"); } catch { child.kill("SIGKILL"); }
      }, timeoutMs);
      child.on("close", (code) => {
        clearTimeout(timer);
        const ok = !timedOut && code === 0;
        process.stdout.write(`\n=== ${file}: ${ok ? "ok" : "FAILED"}\n${Buffer.concat(out)}`);
        if (timedOut) console.error(`TIMED OUT: ${file} ran longer than ${Math.round(timeoutMs / 1000)}s and was killed`);
        if (!ok) failed.push(file);
        resolve();
      });
    });
  await Promise.all(Array.from({ length: Math.min(jobs, queue.length) }, async () => {
    for (let f = queue.shift(); f; f = queue.shift()) await runOne(f);
  }));
  return failed;
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
    const failed = await runFiles(files, { extra });
    if (failed.length) console.error(`${failed.length} file(s) failed: ${failed.join(", ")}`);
    process.exit(failed.length ? 1 : 0);
  }
}
