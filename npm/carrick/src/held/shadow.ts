// The daemon's one call into the compiler engine: `write_shadow` (D4, D5). The
// engine builds the service's program the way the scan does, writes the shadow
// tree a held-open compiler reads, and exits, so its memory is returned.

import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { defaultExists, isRecord, realPath } from "../fs-util.ts";
import { writeAtomic } from "../hook/reuse.ts";
import { checkoutOf } from "../hook/worktree.ts";
import { git, trackedPaths } from "../init/repo-copies.ts";
import { packageRoot, resolveSidecarDir } from "../native.ts";
import { askEngine } from "../engine-ask.ts";
import { heldDir } from "./paths.ts";
import type { ServiceInit } from "./daemon.ts";
import type { GraphqlRootRef, GraphqlTypeTable } from "../facts/store.ts";

/** The three compiler options that change whether one type is assignable to another (the engine's `FitOptions`). */
export type FitOptions = { strictNullChecks: boolean; strictFunctionTypes: boolean; exactOptionalPropertyTypes: boolean };

/** What the daemon reads of `write_shadow`'s answer. */
export type ShadowAnswer = {
  /** Absolute path of the generated `w/tsconfig.json`. */
  tsconfig: string;
  /** The service's own files, relative to the scan root. */
  own_files: string[];
  /** The native TypeScript 7 executable, or null when only TypeScript 6 is installed (D12). */
  native_tsc: string | null;
  /** The judge's wire declarations and the one rule type `__carrick_Wire<S, E>` (S1). */
  wire: string[];
  /** The judge's other tolerances: the helper types a check composes (S1). The literal-widened one is an export suffix of a stub, and a laptop has no stub. */
  tolerances: { unwidened_alias_suffix: string; declarations: string[] };
  /** The options that decide whether one type fits another: as the shadow program was written, and as the judge checks. */
  options: { service: FitOptions; judge: FitOptions };
  /**
   * Present when the request carried the service's GraphQL table: the judge's
   * GraphQL rules (`lines`), the one table declaration they read (`types_decl`),
   * and the leaf of each root the judge reads in its scalar form, by
   * `Type.field`. A root with no leaf is the judge's JSON form (S-G1).
   */
  graphql?: { lines: string[]; types_decl: string | null; leaves: Record<string, unknown> };
  /** The judge's message rules a socket check appends beside `tolerances` (`__carrick_MsgSent<S, E>`); absent from an engine built before them. */
  message?: string[];
};

/** What `write_shadow` is told of the service's GraphQL: its SDL table and the roots its rows answer. */
export type ShadowGraphql = { types: GraphqlTypeTable; roots: GraphqlRootRef[] };

const SHADOW_TIMEOUT_MS = 10 * 60_000;

/** The built compiler engine's entry, or null when none is built. */
export function engineEntry(env: NodeJS.ProcessEnv = process.env): string | null {
  const dirs = [resolveSidecarDir({ env }), path.resolve(packageRoot(), "..", "..", "src", "sidecar")];
  for (const dir of dirs) {
    if (dir === null) continue;
    const entry = path.join(dir, "dist", "src", "index.js");
    if (defaultExists(entry)) return entry;
  }
  return null;
}

/**
 * Run the compiler engine once: `init` as the scan sends it for this service,
 * then `write_shadow` into `out`. The process runs from `~/.carrick/held/`,
 * outside every checkout, because the loader resolves `types` references from
 * its working directory.
 */
export async function writeShadow(entry: string, init: ServiceInit, out: string, env: NodeJS.ProcessEnv, home?: string, graphql?: ShadowGraphql): Promise<ShadowAnswer> {
  const cwd = heldDir(home);
  fs.mkdirSync(cwd, { recursive: true, mode: 0o700 });
  const frames = await askEngine(
    entry,
    [
      {
        action: "init",
        request_id: "init",
        repo_root: init.sidecar_root,
        scan_root: init.scan_root,
        ...(init.tsconfig === null ? {} : { tsconfig_path: init.tsconfig }),
      },
      { action: "write_shadow", request_id: "shadow", out_dir: out, ...(graphql === undefined ? {} : { graphql }) },
    ],
    { cwd, env, timeoutMs: SHADOW_TIMEOUT_MS },
  );
  const answer = frames.get("shadow");
  if (answer === undefined) throw new Error("no write_shadow answer");
  if (answer.status !== "success") throw new Error(`write_shadow failed: ${(answer.errors ?? []).join("; ")}`);
  return answer as unknown as ShadowAnswer;
}

// ---------------------------------------------------------------------------
// A worktree's shadow from main's (carrick#5038, T1). Main's engine writes its
// shadow once; a linked worktree gets a clone of `w/` with its own files
// mirrored over it, and reads the packages in `v/` where main left them.
// ---------------------------------------------------------------------------

/** Main's `ShadowAnswer` and its stamp, saved beside its shadow (`<out>/answer.json`) when the shadow is written. */
export const SAVED_ANSWER_FILE = "answer.json";

export function savedAnswerPath(out: string): string {
  return path.join(out, SAVED_ANSWER_FILE);
}

/** What the shadow was written from: a clone is allowed only while all of it still holds. */
export type ShadowStamp = {
  /** The main checkout's `HEAD` when the shadow was written. */
  main_head: string;
  /** Hash of the main checkout's dependency inputs in the service's scope (`dependencyInputs`). */
  inputs_hash: string;
  /** `heldBuildId()` of the carrick that wrote it. */
  build_id: string;
  /** The main checkout's top folder (real path). */
  main_top: string;
  /** The scan root under the checkout top, `/`-separated; empty when the service is the whole checkout. */
  scan_rel: string;
  /** A file in `v/` that imports a file in `w/`, or null. A clone would leave that import on main's own files. */
  v_reaches_w: string | null;
};

type SavedShadow = { stamp: ShadowStamp; answer: ShadowAnswer };

export type CloneRefusal =
  | "no_saved_answer"
  | "stale_main_inputs"
  | "stale_build"
  | "worktree_deps_differ"
  | "vendored_reaches_own"
  | "vendored_repo_file_differs"
  | "worktree_unreadable"
  | "destination_in_main";

/** A clone that was not made, with the one reason and what it names. */
export type CloneRefused = { refused: CloneRefusal; detail: string };

export type CloneOptions = {
  /** `heldBuildId()` now; compared with the stamp's. */
  buildId: string;
  /** `clone` asks the file system for copy-on-write files and falls back to a plain copy; `plain` never asks. */
  mode?: "clone" | "plain";
  /** The tree copy, with the `fs.constants.COPYFILE_*` mode (0 for a plain copy). Injected by tests. */
  copyTree?: (from: string, to: string, mode: number) => void;
};

const SKIPPED_DIRS = new Set(["node_modules", ".git", ".carrick"]);
/** Left out of the dependency inputs as `is_package_input` leaves them out (`src/change_scope.rs`). */
const NOT_DEPENDENCY_INPUTS = new Set(["carrick.json", ".gitignore", ".gitmodules", ".gitattributes"]);
const TEXT_FILE = /\.(?:d\.)?[cm]?[jt]sx?$/;

type PublishedInputs = { names: Set<string>; extensions: Set<string> };

/** The published list of files a scan reads and the source extensions (`carrickInputs` in this package's `package.json`). */
function publishedInputs(): PublishedInputs {
  const manifest: unknown = JSON.parse(fs.readFileSync(path.join(packageRoot(), "package.json"), "utf8"));
  const inputs = isRecord(manifest) ? manifest.carrickInputs : undefined;
  const list = (key: string): string[] => (isRecord(inputs) && Array.isArray(inputs[key]) ? inputs[key].filter((v): v is string => typeof v === "string") : []);
  const names = list("repo_input_names").filter((name) => !NOT_DEPENDENCY_INPUTS.has(name));
  if (names.length === 0) throw new Error("carrickInputs.repo_input_names is missing from package.json");
  return { names: new Set(names), extensions: new Set(list("source_extensions")) };
}

const isDependencyInput = (name: string, names: Set<string>): boolean => names.has(name) || /^(?:tsconfig|jsconfig)[^/]*\.json$/.test(name) || name === "import_map.json";

const sha = (file: string): string => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

const posix = (rel: string): string => rel.split(path.sep).join("/");

/** `scan_rel` as stored: `/`-separated, no leading `./`, no trailing slash. */
function normalRel(rel: string): string {
  const clean = posix(path.normalize(rel)).replace(/\/+$/, "");
  return clean === "." ? "" : clean;
}

/**
 * A folder that is another checkout (it holds a `.git` file or folder, as the scan
 * skips a git checkout inside a service, carrick#1906), or the place linked
 * worktrees live (`.claude/worktrees`). The folder being scanned is never one.
 */
function isNestedCheckout(dir: string, top: string): boolean {
  return defaultExists(path.join(dir, ".git")) || posix(path.relative(top, dir)).endsWith(".claude/worktrees");
}

type TreeScan = {
  /** Dependency inputs in the service's scope, by path under the checkout top, with the hash of their bytes. */
  inputs: Map<string, string>;
  /** The service's source files, by path under the scan root. */
  sources: Set<string>;
};

/**
 * The service's dependency inputs and source files in one checkout: guarded
 * files anywhere in the service's folder, and in each folder above it up to the
 * checkout top (PLAN.md R3). Folders named `node_modules`, `.git` and `.carrick`
 * are not read. Read from the disk, no git run.
 */
function scanTree(top: string, scanRel: string, published: PublishedInputs): TreeScan {
  const inputs = new Map<string, string>();
  const sources = new Set<string>();
  const service = path.join(top, scanRel);
  const walk = (dir: string): void => {
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const abs = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (!SKIPPED_DIRS.has(entry.name) && !isNestedCheckout(abs, top)) walk(abs);
      } else if (entry.isFile()) {
        if (isDependencyInput(entry.name, published.names)) inputs.set(posix(path.relative(top, abs)), sha(abs));
        if (published.extensions.has(path.extname(entry.name).slice(1))) sources.add(posix(path.relative(service, abs)));
      }
    }
  };
  walk(service);
  const stop = path.resolve(top);
  if (scanRel !== "") {
    for (let dir = path.dirname(service); ; dir = path.dirname(dir)) {
      for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        if (entry.isFile() && isDependencyInput(entry.name, published.names)) inputs.set(posix(path.relative(top, path.join(dir, entry.name))), sha(path.join(dir, entry.name)));
      }
      if (dir === stop || dir === path.dirname(dir)) break;
    }
  }
  return { inputs, sources };
}

function hashInputs(inputs: Map<string, string>): string {
  const hash = createHash("sha256");
  for (const rel of [...inputs.keys()].sort()) hash.update(`${rel}\0${inputs.get(rel)}\n`);
  return hash.digest("hex");
}

/** The first path whose hash differs between two input maps, or one present on one side only; null when equal. */
function firstDifference(a: Map<string, string>, b: Map<string, string>): string | null {
  for (const rel of [...new Set([...a.keys(), ...b.keys()])].sort()) if (a.get(rel) !== b.get(rel)) return rel;
  return null;
}

/** The first file in `v/` that holds a relative specifier resolving into `w/`, or null. */
function vendoredReachingOwn(out: string): string | null {
  const w = path.join(out, "w");
  let found: string | null = null;
  const walk = (dir: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (found !== null) return;
      const abs = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(abs);
      else if (entry.isFile() && TEXT_FILE.test(entry.name)) {
        for (const match of fs.readFileSync(abs, "utf8").matchAll(/["'`](\.{1,2}\/[^"'`\n]*)["'`]/g)) {
          if (path.resolve(path.dirname(abs), match[1]!).startsWith(w + path.sep)) {
            found = abs;
            return;
          }
        }
      }
    }
  };
  const v = path.join(out, "v");
  if (defaultExists(v)) walk(v);
  return found;
}

/**
 * Save main's answer and its stamp beside its shadow, right after the shadow is
 * written. Returns whether it landed; false (and no file) outside a git checkout
 * or when nothing can be written, so a start never fails over it.
 */
export function writeShadowStamp(out: string, answer: ShadowAnswer, scanRoot: string, buildId: string): boolean {
  try {
    const top = checkoutOf(scanRoot)?.top;
    if (top === undefined) return false;
    const head = git(top, ["rev-parse", "HEAD"])?.trim();
    if (head === undefined || head === "") return false;
    const scanRel = normalRel(path.relative(top, realPath(scanRoot)));
    const stamp: ShadowStamp = {
      main_head: head,
      inputs_hash: hashInputs(scanTree(top, scanRel, publishedInputs()).inputs),
      build_id: buildId,
      main_top: top,
      scan_rel: scanRel,
      v_reaches_w: vendoredReachingOwn(out),
    };
    return writeAtomic(savedAnswerPath(out), `${JSON.stringify({ stamp, answer } satisfies SavedShadow, null, 1)}\n`);
  } catch {
    return false;
  }
}

function readSaved(out: string): SavedShadow | null {
  try {
    const saved: unknown = JSON.parse(fs.readFileSync(savedAnswerPath(out), "utf8"));
    if (!isRecord(saved) || !isRecord(saved.stamp) || !isRecord(saved.answer)) return null;
    const stamp = saved.stamp;
    for (const key of ["main_head", "inputs_hash", "build_id", "main_top", "scan_rel"] as const) if (typeof stamp[key] !== "string") return null;
    if (typeof saved.answer.tsconfig !== "string" || !Array.isArray(saved.answer.own_files)) return null;
    return saved as unknown as SavedShadow;
  } catch {
    return null;
  }
}

const sameBytes = (a: string, b: string): boolean => {
  try {
    return fs.readFileSync(a).equals(fs.readFileSync(b));
  } catch {
    return false;
  }
};

/** Every repo file main vendored into `v/` (a path under main's top that is not under `node_modules`), as repo-relative paths. */
function vendoredRepoFiles(out: string, mainTop: string): string[] {
  const found: string[] = [];
  const walk = (dir: string, rel: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (SKIPPED_DIRS.has(entry.name)) continue;
      if (entry.isDirectory()) walk(path.join(dir, entry.name), posix(path.join(rel, entry.name)));
      else if (entry.isFile()) found.push(posix(path.join(rel, entry.name)));
    }
  };
  const root = path.join(out, "v", mainTop);
  if (defaultExists(root)) walk(root, "");
  return found;
}

/** Every string in a parsed config with the shadow's own folder as main wrote it moved to the clone's. */
function moveOwn(value: unknown, fromW: string[], toW: string): unknown {
  if (typeof value === "string") {
    for (const from of fromW) if (value === from || value.startsWith(from + path.sep)) return toW + value.slice(from.length);
    return value;
  }
  if (Array.isArray(value)) return value.map((v) => moveOwn(v, fromW, toW));
  if (isRecord(value)) return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, moveOwn(v, fromW, toW)]));
  return value;
}

const insideOf = (parent: string, child: string): boolean => {
  const rel = path.relative(parent, child);
  return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel));
};

/**
 * A linked worktree's shadow, cloned from main's (carrick#5038, T1).
 *
 * `mainOut` is main's shadow folder (`w/`, `v/`, `answer.json`); `worktreeTop`
 * the worktree's top folder; `scanRel` the service's scan root under a checkout
 * top; `out` where the clone goes, given by the caller. The result is the
 * `ShadowAnswer` of the clone: `w/` cloned from main's (copy-on-write where the
 * file system has it, a plain copy where not), the worktree's own files mirrored
 * over it (changed, added and deleted, relative to the scan root), and
 * `w/tsconfig.json` moved to the clone's own files. `v/` is not copied: the
 * clone's tsconfig names main's packages by their path in `mainOut`.
 *
 * It refuses, with the one reason and what it names, and writes nothing, when
 * main's stamp is missing or stale (dependency inputs, carrick build),
 * when a dependency input of the worktree differs from main's (PLAN.md R3),
 * when a repo file main vendored differs in the worktree (R10), or when `v/`
 * imports `w/`. `mainOut` is never written.
 */
export function cloneShadow(mainOut: string, worktreeTop: string, scanRel: string, out: string, options: CloneOptions): ShadowAnswer | CloneRefused {
  const refuse = (refused: CloneRefusal, detail: string): CloneRefused => ({ refused, detail });
  const saved = readSaved(mainOut);
  if (saved === null) return refuse("no_saved_answer", `${savedAnswerPath(mainOut)} is missing or unreadable`);
  const { stamp, answer } = saved;
  const rel = normalRel(scanRel);
  if (insideOf(realPath(mainOut), realPath(out))) return refuse("destination_in_main", out);
  // Main's HEAD is not compared (`main_head` is kept as a record): every own file git tracks is mirrored from the worktree, so a
  // commit in main changes nothing the clone reads from main but `v/`, which the dependency inputs and the build id guard.
  if (options.buildId !== stamp.build_id) return refuse("stale_build", "the shadow was written by another carrick build");
  let published: PublishedInputs;
  let main: TreeScan;
  let worktree: TreeScan;
  try {
    published = publishedInputs();
    main = scanTree(stamp.main_top, stamp.scan_rel, published);
    worktree = scanTree(worktreeTop, rel, published);
  } catch (error) {
    return refuse("worktree_unreadable", String(error));
  }
  if (hashInputs(main.inputs) !== stamp.inputs_hash) return refuse("stale_main_inputs", "a dependency input of main changed since the shadow was written");
  const differs = firstDifference(main.inputs, worktree.inputs);
  if (differs !== null) return refuse("worktree_deps_differ", differs);
  if (stamp.v_reaches_w !== null) return refuse("vendored_reaches_own", stamp.v_reaches_w);
  for (const file of vendoredRepoFiles(mainOut, stamp.main_top)) {
    if (!sameBytes(path.join(stamp.main_top, file), path.join(worktreeTop, file))) return refuse("vendored_repo_file_differs", file);
  }

  const mainService = path.join(stamp.main_top, stamp.scan_rel);
  const service = path.join(worktreeTop, rel);
  const own = new Set(answer.own_files);
  // An own file the worktree lacks is gone from the clone only when git tracks it in main. One git ignores or does not track
  // (generated clients, codegen output) is never in a fresh worktree, so the clone keeps main's copy.
  const absent = answer.own_files.filter((file) => !defaultExists(path.join(path.join(worktreeTop, rel), file)));
  const tracked = absent.length === 0 ? new Set<string>() : trackedPaths(stamp.main_top, absent.map((file) => path.join(stamp.scan_rel, file)));
  if (tracked === null) return refuse("worktree_unreadable", `git ls-files failed in ${stamp.main_top}`);
  const removed = new Set(absent.filter((file) => tracked.has(path.join(stamp.scan_rel, file))));
  const added = [...worktree.sources].filter((file) => !own.has(file) && !sameBytes(path.join(mainService, file), path.join(service, file))).sort();

  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(out, { recursive: true, mode: 0o700 });
  const outW = path.join(out, "w");
  const copyTree = options.copyTree ?? ((from, to, mode) => fs.cpSync(from, to, { recursive: true, mode }));
  try {
    if (options.mode === "plain") throw new Error("plain copy asked for");
    copyTree(path.join(mainOut, "w"), outW, fs.constants.COPYFILE_FICLONE);
  } catch {
    fs.rmSync(outW, { recursive: true, force: true });
    copyTree(path.join(mainOut, "w"), outW, 0);
  }

  const deleted: string[] = [];
  for (const file of own) {
    const from = path.join(service, file);
    const to = path.join(outW, file);
    if (removed.has(file)) {
      fs.rmSync(to, { force: true });
      deleted.push(file);
    } else if (defaultExists(from) && !sameBytes(from, to)) {
      fs.writeFileSync(to, fs.readFileSync(from));
    }
  }
  for (const file of added) {
    fs.mkdirSync(path.dirname(path.join(outW, file)), { recursive: true });
    fs.writeFileSync(path.join(outW, file), fs.readFileSync(path.join(service, file)));
  }

  const mainW = [path.join(mainOut, "w"), path.join(realPath(mainOut), "w")];
  const tsconfig = path.join(outW, "tsconfig.json");
  const config = moveOwn(JSON.parse(fs.readFileSync(tsconfig, "utf8")), mainW, outW) as { files?: string[] };
  const gone = new Set(deleted.map((file) => path.join(outW, file)));
  config.files = [...new Set([...(config.files ?? []).filter((file) => !gone.has(file)), ...added.map((file) => path.join(outW, file))])];
  fs.writeFileSync(tsconfig, JSON.stringify(config, null, 1));

  const gonePaths = new Set(deleted);
  return { ...answer, tsconfig, own_files: [...answer.own_files.filter((file) => !gonePaths.has(file)), ...added].sort() };
}
