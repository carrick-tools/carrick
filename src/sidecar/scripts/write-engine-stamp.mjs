// Writes dist/src/engine-stamp.json: a sha256 over the inputs that shape the
// compiler engine's build (package.json, package-lock.json, tsconfig*.json and
// src/**). Tests are left out on purpose, so editing a test never makes a good
// build look stale. The scanner re-hashes the same inputs on the
// source-checkout paths and refuses an engine that does not match; the hash
// must therefore stay byte-identical with `src/engine_stamp.rs`
// (`hash_inputs`). Reference: src/sidecar/README.md, "Build stamp". Run by `npm run build`.
//
// Usage: node write-engine-stamp.mjs [engine-root] [stamp-file]
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

function walk(dir, rel, out) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const entryRel = `${rel}/${entry.name}`;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, entryRel, out);
    else if (fs.statSync(full).isFile()) out.push(entryRel);
  }
}

export function inputFiles(root) {
  const files = [];
  for (const entry of fs.readdirSync(root, { withFileTypes: true })) {
    const name = entry.name;
    const fixed = name === "package.json" || name === "package-lock.json";
    if ((fixed || /^tsconfig.*\.json$/.test(name)) && fs.statSync(path.join(root, name)).isFile()) {
      files.push(name);
    }
  }
  if (fs.existsSync(path.join(root, "src"))) walk(path.join(root, "src"), "src", files);
  return files.sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
}

export function hashInputs(root) {
  const hash = createHash("sha256");
  for (const rel of inputFiles(root)) {
    const bytes = fs.readFileSync(path.join(root, rel));
    hash.update(`${rel}\0${bytes.length}\0`);
    hash.update(bytes);
  }
  return hash.digest("hex");
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const root = path.resolve(process.argv[2] ?? path.join(path.dirname(fileURLToPath(import.meta.url)), ".."));
  const out = path.resolve(process.argv[3] ?? path.join(root, "dist", "src", "engine-stamp.json"));
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.writeFileSync(out, `${JSON.stringify({ inputs_sha256: hashInputs(root) })}\n`);
}
