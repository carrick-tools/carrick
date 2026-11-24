// T1 of the worktree check (carrick#5038): a worktree's shadow is a clone of the
// main checkout's shadow `w/` with the worktree's own files mirrored over it.
// A hand-made shadow in a temp dir; invented names.

import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { cloneShadow, savedAnswerPath, writeShadowStamp, type CloneRefused, type ShadowAnswer } from "../src/held/shadow.ts";

const BUILD = "build-1";
const SCAN_REL = "services/orders";

type Bed = { base: string; main: string; wt: string; mainOut: string; out: string; answer: ShadowAnswer; pkgFile: string };

function git(cwd: string, ...args: string[]): string {
  return execFileSync("git", ["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false", ...args], { cwd, encoding: "utf8" }).trim();
}

function put(file: string, text: string): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

/** Every file under `dir` with its hash, for "main was not touched". */
function treeHash(dir: string): Record<string, string> {
  const out: Record<string, string> = {};
  const walk = (current: string): void => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const abs = path.join(current, entry.name);
      if (entry.isDirectory()) walk(abs);
      else out[path.relative(dir, abs)] = createHash("sha256").update(fs.readFileSync(abs)).digest("hex");
    }
  };
  walk(dir);
  return out;
}

/** A main checkout, one linked worktree cut from it, and the shadow main's engine would have written. */
function bed(extra: { vendoredReachesOwn?: boolean } = {}): Bed {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "clone-shadow-")));
  const main = path.join(base, "main");
  const svc = path.join(main, SCAN_REL);
  put(path.join(main, "package.json"), '{"name":"top","private":true}\n');
  put(path.join(svc, "package.json"), '{"name":"orders"}\n');
  put(path.join(svc, "tsconfig.json"), '{"compilerOptions":{"strict":true}}\n');
  put(path.join(svc, "src/a.ts"), 'import { b } from "./b.ts";\nexport const a = b;\n');
  put(path.join(svc, "src/b.ts"), "export const b = { id: 1 };\n");
  put(path.join(svc, "src/d.ts"), "export const d = 4;\n");
  put(path.join(svc, ".gitignore"), "node_modules\n");
  git(main, "init", "-q", "-b", "main");
  git(main, "add", "-A");
  git(main, "commit", "-q", "-m", "init");
  const wt = path.join(base, "wt");
  git(main, "worktree", "add", "-q", "-b", "agent", wt);

  // What main's engine wrote: w/ holds the own files by their path under the scan root, v/ holds the packages.
  const mainOut = path.join(base, "held", "svc-1");
  const w = path.join(mainOut, "w");
  const pkgFile = path.join(mainOut, "v", "pkgs", "pkg", "index.d.ts");
  for (const rel of ["src/a.ts", "src/b.ts", "src/d.ts"]) put(path.join(w, rel), fs.readFileSync(path.join(svc, rel), "utf8"));
  put(pkgFile, extra.vendoredReachesOwn ? 'export * from "../../../w/src/b.js";\n' : "export declare const pkg: number;\n");
  put(
    path.join(w, "tsconfig.json"),
    JSON.stringify(
      {
        compilerOptions: { moduleResolution: "bundler", paths: { pkg: [pkgFile], "own-b": [path.join(w, "src/b.ts")] } },
        files: [path.join(w, "src/a.ts"), path.join(w, "src/b.ts"), path.join(w, "src/d.ts"), pkgFile],
        include: [],
      },
      null,
      1,
    ),
  );
  const answer: ShadowAnswer = {
    tsconfig: path.join(w, "tsconfig.json"),
    own_files: ["src/a.ts", "src/b.ts", "src/d.ts"],
    native_tsc: "/opt/tsc",
    wire: ["type __carrick_Wire<S, E> = S;"],
    tolerances: { unwidened_alias_suffix: "x", declarations: [] },
    options: {
      service: { strictNullChecks: true, strictFunctionTypes: true, exactOptionalPropertyTypes: false },
      judge: { strictNullChecks: true, strictFunctionTypes: true, exactOptionalPropertyTypes: false },
    },
  };
  assert.equal(writeShadowStamp(mainOut, answer, svc, BUILD), true);
  return { base, main, wt, mainOut, out: path.join(base, "held", "wt-1"), answer, pkgFile };
}

function clone(b: Bed, more: Partial<Parameters<typeof cloneShadow>[4]> = {}): ShadowAnswer | CloneRefused {
  return cloneShadow(b.mainOut, b.wt, SCAN_REL, b.out, { buildId: BUILD, ...more });
}

function refused(result: ShadowAnswer | CloneRefused): CloneRefused {
  assert.ok("refused" in result, "expected a refusal");
  return result;
}

function ok(result: ShadowAnswer | CloneRefused): ShadowAnswer {
  assert.ok(!("refused" in result), `expected a clone, got ${JSON.stringify(result)}`);
  return result;
}

const read = (file: string): string => fs.readFileSync(file, "utf8");
const config = (b: Bed): { files: string[]; compilerOptions: { paths: Record<string, string[]> } } => JSON.parse(read(path.join(b.out, "w", "tsconfig.json")));

test("a changed file is mirrored and an untouched one equals main's", () => {
  const b = bed();
  const svc = path.join(b.wt, SCAN_REL);
  put(path.join(svc, "src/b.ts"), "export const b = { id: 1, name: 'x' };\n");
  ok(clone(b));
  assert.equal(read(path.join(b.out, "w/src/b.ts")), "export const b = { id: 1, name: 'x' };\n");
  assert.equal(read(path.join(b.out, "w/src/a.ts")), read(path.join(b.mainOut, "w/src/a.ts")));
});

test("an added file is present and named in files", () => {
  const b = bed();
  put(path.join(b.wt, SCAN_REL, "src/c.ts"), "export const c = 3;\n");
  const answer = ok(clone(b));
  assert.equal(read(path.join(b.out, "w/src/c.ts")), "export const c = 3;\n");
  assert.ok(config(b).files.includes(path.join(b.out, "w/src/c.ts")));
  assert.deepEqual(answer.own_files, ["src/a.ts", "src/b.ts", "src/c.ts", "src/d.ts"]);
});

test("a file the worktree deleted is gone from the clone, from files and from own_files", () => {
  const b = bed();
  fs.rmSync(path.join(b.wt, SCAN_REL, "src/d.ts"));
  const answer = ok(clone(b));
  assert.equal(fs.existsSync(path.join(b.out, "w/src/d.ts")), false);
  assert.ok(!config(b).files.some((f) => f.endsWith("/src/d.ts")));
  assert.deepEqual(answer.own_files, ["src/a.ts", "src/b.ts"]);
  assert.equal(fs.existsSync(path.join(b.mainOut, "w/src/d.ts")), true);
});

test("a source file that main's program never loaded, and that the branch did not change, is not copied", () => {
  const b = bed();
  put(path.join(b.main, SCAN_REL, "src/loose.ts"), "export const loose = 0;\n");
  put(path.join(b.wt, SCAN_REL, "src/loose.ts"), "export const loose = 0;\n");
  ok(clone(b));
  assert.equal(fs.existsSync(path.join(b.out, "w/src/loose.ts")), false);
});

test("v/ is main's: not copied, not written, and the clone points at it", () => {
  const b = bed();
  const before = treeHash(b.mainOut);
  put(path.join(b.wt, SCAN_REL, "src/b.ts"), "export const b = { id: 2 };\n");
  put(path.join(b.wt, SCAN_REL, "src/c.ts"), "export const c = 3;\n");
  fs.rmSync(path.join(b.wt, SCAN_REL, "src/d.ts"));
  ok(clone(b));
  assert.equal(fs.existsSync(path.join(b.out, "v")), false);
  assert.ok(config(b).files.includes(b.pkgFile));
  assert.deepEqual(config(b).compilerOptions.paths.pkg, [b.pkgFile]);
  const after = treeHash(b.mainOut);
  const stamped = Object.keys(after).filter((k) => !(k in before));
  assert.deepEqual(stamped, []);
  assert.deepEqual(after, before);
});

test("tsconfig: own entries move to out/w and package entries stay on main's v/", () => {
  const b = bed();
  const answer = ok(clone(b));
  const tsconfig = config(b);
  assert.equal(answer.tsconfig, path.join(b.out, "w", "tsconfig.json"));
  assert.deepEqual(tsconfig.compilerOptions.paths["own-b"], [path.join(b.out, "w/src/b.ts")]);
  assert.deepEqual(tsconfig.compilerOptions.paths.pkg, [b.pkgFile]);
  assert.ok(tsconfig.files.includes(path.join(b.out, "w/src/a.ts")));
  assert.ok(!tsconfig.files.some((f) => f.startsWith(path.join(b.mainOut, "w"))));
  assert.deepEqual({ ...answer, tsconfig: "", own_files: [] }, { ...b.answer, tsconfig: "", own_files: [] });
});

test("main's shadow is never written, even by a clone that mirrors changes", () => {
  const b = bed();
  const before = treeHash(b.mainOut);
  put(path.join(b.wt, SCAN_REL, "src/a.ts"), "export const a = 9;\n");
  ok(clone(b));
  assert.deepEqual(treeHash(b.mainOut), before);
  assert.match(refused(cloneShadow(b.mainOut, b.wt, SCAN_REL, path.join(b.mainOut, "inside"), { buildId: BUILD })).refused, /destination_in_main/);
});

test("a main commit since the stamp is refused", () => {
  const b = bed();
  put(path.join(b.main, SCAN_REL, "src/e.ts"), "export const e = 5;\n");
  git(b.main, "add", "-A");
  git(b.main, "commit", "-q", "-m", "next");
  assert.equal(refused(clone(b)).refused, "stale_main_head");
});

test("a changed dependency input in main since the stamp is refused", () => {
  const b = bed();
  put(path.join(b.main, "package.json"), '{"name":"top","private":true,"dependencies":{"x":"1"}}\n');
  assert.equal(refused(clone(b)).refused, "stale_main_inputs");
});

test("a different carrick build is refused", () => {
  const b = bed();
  assert.equal(refused(clone(b, { buildId: "build-2" })).refused, "stale_build");
});

test("a missing or unreadable saved answer is refused", () => {
  const b = bed();
  fs.rmSync(savedAnswerPath(b.mainOut));
  assert.equal(refused(clone(b)).refused, "no_saved_answer");
  put(savedAnswerPath(b.mainOut), "{not json");
  assert.equal(refused(clone(b)).refused, "no_saved_answer");
});

test("a worktree dependency input that differs is refused, whichever folder holds it", () => {
  for (const [rel, text] of [
    [path.join(SCAN_REL, "package.json"), '{"name":"orders","dependencies":{"y":"2"}}\n'],
    [path.join(SCAN_REL, "tsconfig.json"), '{"compilerOptions":{"strict":false}}\n'],
    ["package.json", '{"name":"top","private":true,"workspaces":["*"]}\n'],
    [path.join(SCAN_REL, "pnpm-lock.yaml"), "lockfileVersion: 9\n"],
    [path.join(SCAN_REL, "src/tsconfig.build.json"), "{}\n"],
  ] as const) {
    const b = bed();
    put(path.join(b.wt, rel), text);
    const r = refused(clone(b));
    assert.equal(r.refused, "worktree_deps_differ", rel);
    assert.ok(r.detail.includes(rel.split(path.sep).join("/")), r.detail);
    assert.equal(fs.existsSync(b.out), false);
  }
});

test("a dependency input outside the service's folders is not part of the guard", () => {
  const b = bed();
  put(path.join(b.wt, "other-service", "package.json"), '{"name":"other"}\n');
  put(path.join(b.wt, ".gitignore"), "x\n");
  ok(clone(b));
});

test("a v/ file that imports a w/ file is refused", () => {
  const b = bed({ vendoredReachesOwn: true });
  assert.equal(refused(clone(b)).refused, "vendored_reaches_own");
});

test("a repo file main vendored that the worktree changed is refused", () => {
  const b = bed();
  const lib = path.join(b.main, "libs/shared/index.ts");
  put(lib, "export const s = 1;\n");
  git(b.main, "add", "-A");
  git(b.main, "commit", "-q", "-m", "lib");
  git(b.wt, "merge", "-q", "main");
  put(path.join(b.mainOut, "v", b.main, "libs/shared/index.ts"), "export const s = 1;\n");
  // Stamp again: main now holds a vendored repo file and a new HEAD.
  assert.equal(writeShadowStamp(b.mainOut, b.answer, path.join(b.main, SCAN_REL), BUILD), true);
  ok(clone(b));
  put(path.join(b.wt, "libs/shared/index.ts"), "export const s = 2;\n");
  assert.equal(refused(clone(b)).refused, "vendored_repo_file_differs");
});

test("with no copy-on-write clone the plain copy gives the same tree", () => {
  const b = bed();
  put(path.join(b.wt, SCAN_REL, "src/b.ts"), "export const b = { id: 3 };\n");
  const modes: number[] = [];
  const failingClone = (from: string, to: string, mode: number): void => {
    modes.push(mode);
    if (mode !== 0) throw Object.assign(new Error("clone is not supported here"), { code: "ENOTSUP" });
    fs.cpSync(from, to, { recursive: true });
  };
  ok(clone(b, { copyTree: failingClone }));
  assert.deepEqual(modes.length, 2, "a clone try and then a plain copy");
  assert.notEqual(modes[0], 0);
  assert.equal(modes[1], 0);
  assert.equal(read(path.join(b.out, "w/src/b.ts")), "export const b = { id: 3 };\n");
  assert.equal(read(path.join(b.out, "w/src/a.ts")), read(path.join(b.mainOut, "w/src/a.ts")));
  assert.deepEqual(config(b).compilerOptions.paths["own-b"], [path.join(b.out, "w/src/b.ts")]);
});

test("the copy mode 'plain' never asks for a clone", () => {
  const b = bed();
  const modes: number[] = [];
  ok(clone(b, { mode: "plain", copyTree: (from, to, mode) => (modes.push(mode), fs.cpSync(from, to, { recursive: true })) }));
  assert.deepEqual(modes, [0]);
});

test("the default copy works on this machine's file system", () => {
  const b = bed();
  ok(clone(b));
  assert.equal(read(path.join(b.out, "w/src/a.ts")), read(path.join(b.mainOut, "w/src/a.ts")));
});

test("the stamp file records main's head, its inputs, the build and the answer", () => {
  const b = bed();
  const saved = JSON.parse(read(savedAnswerPath(b.mainOut)));
  assert.equal(saved.stamp.main_head, git(b.main, "rev-parse", "HEAD"));
  assert.equal(saved.stamp.build_id, BUILD);
  assert.equal(saved.stamp.main_top, b.main);
  assert.equal(saved.stamp.scan_rel, SCAN_REL);
  assert.match(saved.stamp.inputs_hash, /^[0-9a-f]{64}$/);
  assert.deepEqual(saved.answer, b.answer);
  assert.equal(path.basename(savedAnswerPath(b.mainOut)), "answer.json");
});

test("no stamp is written outside a git checkout", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "clone-shadow-nogit-"));
  const out = path.join(dir, "out");
  fs.mkdirSync(path.join(out, "w"), { recursive: true });
  assert.equal(writeShadowStamp(out, bed().answer, dir, BUILD), false);
  assert.equal(fs.existsSync(savedAnswerPath(out)), false);
});
