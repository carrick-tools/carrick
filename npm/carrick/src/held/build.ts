// The held daemon's build id: what a client compares with the daemon it finds,
// so a daemon left running across a package upgrade never answers with old
// rules. A version string would not do: a relinked engine or a dev build
// changes the rules without changing it.

import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { engineEntry } from "./shadow.ts";

export type BuildInputs = {
  /** The package's own built files the daemon loads: this directory's parent (`dist/`, or `src/` in a dev tree). */
  own: string;
  /** The compiler engine's built files (`<sidecar>/dist/src`), or null when none is built. */
  engine: string | null;
  /** The TypeScript 7 executable the held compiler runs, or null when only TypeScript 6 is installed. */
  tsc: string | null;
};

function filesUnder(dir: string): string[] {
  const out: string[] = [];
  const walk = (current: string): void => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const abs = path.join(current, entry.name);
      if (entry.isDirectory()) walk(abs);
      else if (entry.isFile()) out.push(abs);
    }
  };
  walk(dir);
  return out.map((file) => path.relative(dir, file).split(path.sep).join("/")).sort();
}

const sha = (file: string): string => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

/**
 * The native executable beside the engine: the first
 * `node_modules/@typescript/typescript-<platform>-<arch>/lib/tsc` at or above
 * the engine's package root (the engine's own lookup for the compiler it holds).
 */
export function nativeTscBeside(engineDir: string, platform: string = process.platform, arch: string = process.arch): string | null {
  const rel = path.join("node_modules", "@typescript", `typescript-${platform}-${arch}`, "lib", platform === "win32" ? "tsc.exe" : "tsc");
  for (let dir = path.resolve(engineDir, "..", ".."); ; dir = path.dirname(dir)) {
    const candidate = path.join(dir, rel);
    if (fs.existsSync(candidate)) return candidate;
    if (path.dirname(dir) === dir) return null;
  }
}

export function buildInputs(env: NodeJS.ProcessEnv = process.env): BuildInputs {
  const entry = engineEntry(env);
  const engine = entry === null ? null : path.dirname(entry);
  return {
    own: path.resolve(path.dirname(fileURLToPath(import.meta.url)), ".."),
    engine,
    tsc: engine === null ? null : nativeTscBeside(engine),
  };
}

/** sha-256 over the bytes of every input file, by group and relative path in sorted order. */
export function heldBuildId(inputs: BuildInputs = buildInputs()): string {
  const hash = createHash("sha256");
  for (const [group, dir] of [["own", inputs.own], ["engine", inputs.engine]] as const) {
    if (dir === null) continue;
    for (const rel of filesUnder(dir)) hash.update(`${group}:${rel}\0${sha(path.join(dir, rel))}\n`);
  }
  if (inputs.tsc !== null) hash.update(`tsc\0${sha(inputs.tsc)}\n`);
  return hash.digest("hex");
}
