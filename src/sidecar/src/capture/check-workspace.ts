/**
 * Scratch synthetic-monorepo assembly for the v2 check phase.
 *
 * Copies each capture stub package into a temp pnpm workspace
 * (node-linker=isolated, so every stub keeps its OWN node_modules and two
 * versions of one dependency genuinely coexist), adds a `carrick-probes`
 * package that depends on every stub via `workspace:*`, writes the checker
 * tsconfig, and computes semver-dedupe overrides so patch/minor drift on a
 * private-member class does not manufacture a nominal false-incompatible
 * (design Check step 6) while genuinely conflicting majors stay physically
 * duplicated (and thus verdict incompatible, correctly).
 *
 * Seam: node builtins + this bundle only.
 */

import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import * as path from 'node:path';
import type { CheckStubInput } from './api.js';
import type { ProbePlan } from './check-probe.js';
import { WriteGuard } from './guarded-fs.js';
import { RESOLUTION_FILE, type ResolutionEdges } from './resolution-edges.js';

const PROBES_PACKAGE = 'carrick-probes';

export interface AssembledStub {
  serviceName: string;
  /** Workspace package dir under packages/ (== the @carrick/<dir> suffix). */
  packageDir: string;
  /** Full package specifier, e.g. @carrick/orders-engine. */
  packageName: string;
}

export interface AssembledWorkspace {
  workspaceDir: string;
  /** Every write and the clean-up are held to `workspaceDir` (carrick#1748). */
  guard: WriteGuard;
  probesDir: string; // absolute
  /** Relative (forward-slash) probes dir, e.g. packages/carrick-probes. */
  probesRel: string;
  stubs: AssembledStub[];
  /** service name -> package specifier (for probe imports). */
  packageOf: (serviceName: string) => string;
  /** package dir -> package specifier (for scrub labels). */
  packageLabelOf: (packageDir: string) => string | undefined;
  /** package dir -> service name (for poison attribution). */
  serviceOfPackageDir: (packageDir: string) => string | undefined;
}

interface ParsedVersion {
  major: number;
  minor: number;
  patch: number;
  raw: string;
}

function parseVersion(v: string): ParsedVersion {
  const core = v.replace(/^[^\d]*/, '').split('-')[0].split('+')[0];
  const [maj = '0', min = '0', pat = '0'] = core.split('.');
  return {
    major: Number(maj) || 0,
    minor: Number(min) || 0,
    patch: Number(pat) || 0,
    raw: v,
  };
}

/**
 * Semver compat key: same key => dedupe candidates. 0.x is minor-scoped, and
 * 0.0.x is patch-scoped: semver treats every 0.0.x release as its own
 * breaking boundary, so 0.0.3 and 0.0.5 must never collapse onto one
 * physical copy (that would manufacture a false-compatible for by-reference
 * library types).
 */
function compatKey(v: ParsedVersion): string {
  if (v.major > 0) return `${v.major}`;
  if (v.minor > 0) return `0.${v.minor}`;
  return `0.0.${v.patch}`;
}

function versionGreater(a: ParsedVersion, b: ParsedVersion): boolean {
  if (a.major !== b.major) return a.major > b.major;
  if (a.minor !== b.minor) return a.minor > b.minor;
  return a.patch > b.patch;
}

/**
 * Build pnpm exact-selector overrides that collapse semver-compatible drift to
 * the max version in each compat group, leaving conflicting majors untouched.
 * Returns a deterministically key-sorted map, e.g. { "bson@6.8.0": "6.10.1" }.
 */
export function computeDedupeOverrides(
  stubs: { dependencies: Record<string, string> }[]
): Record<string, string> {
  const byName = new Map<string, Set<string>>();
  for (const stub of stubs) {
    for (const [name, version] of Object.entries(stub.dependencies ?? {})) {
      if (!byName.has(name)) byName.set(name, new Set());
      byName.get(name)!.add(version);
    }
  }

  const overrides: Record<string, string> = {};
  for (const [name, versions] of byName) {
    if (versions.size < 2) continue;
    const groups = new Map<string, ParsedVersion[]>();
    for (const raw of versions) {
      const parsed = parseVersion(raw);
      const key = compatKey(parsed);
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key)!.push(parsed);
    }
    for (const members of groups.values()) {
      if (members.length < 2) continue;
      let max = members[0];
      for (const m of members) if (versionGreater(m, max)) max = m;
      for (const m of members) {
        if (m.raw !== max.raw) overrides[`${name}@${m.raw}`] = max.raw;
      }
    }
  }

  return Object.fromEntries(
    Object.keys(overrides)
      .sort()
      .map((k) => [k, overrides[k]])
  );
}

/** A stub's recorded edges; a stub stored without the file records none. */
function readStubResolution(stubDir: string): ResolutionEdges {
  let parsed: unknown;
  try {
    parsed = JSON.parse(fs.readFileSync(path.join(stubDir, RESOLUTION_FILE), 'utf8'));
  } catch {
    return {};
  }
  const edges = (parsed as { edges?: unknown } | null)?.edges;
  if (!edges || typeof edges !== 'object') return {};
  const out: ResolutionEdges = {};
  for (const [parent, children] of Object.entries(edges as Record<string, unknown>)) {
    if (!children || typeof children !== 'object') continue;
    for (const [child, version] of Object.entries(children as Record<string, unknown>)) {
      if (typeof version === 'string') (out[parent] ??= {})[child] = version;
    }
  }
  return out;
}

/**
 * pnpm parent-scoped overrides (`<parent>@<version>><child>`) that install
 * each edge the stubs recorded at the version the scanned repo installed
 * (carrick#2091). A parent-scoped override wins over a generic one, so an
 * edge's version first passes through the dedupe table: one physical copy per
 * compat group still holds. An edge two stubs record at versions the dedupe
 * does not reconcile is left to the resolver.
 */
export function computeResolutionOverrides(
  edgeSets: ResolutionEdges[],
  dedupe: Record<string, string>
): Record<string, string> {
  const versionsOf = new Map<string, Set<string>>();
  for (const edges of edgeSets) {
    for (const [parent, children] of Object.entries(edges)) {
      for (const [child, version] of Object.entries(children)) {
        const key = `${parent}>${child}`;
        if (!versionsOf.has(key)) versionsOf.set(key, new Set());
        versionsOf.get(key)!.add(dedupe[`${child}@${version}`] ?? version);
      }
    }
  }
  const overrides: Record<string, string> = {};
  for (const key of [...versionsOf.keys()].sort()) {
    const versions = versionsOf.get(key)!;
    if (versions.size === 1) overrides[key] = [...versions][0];
  }
  return overrides;
}

function readStubPackageName(stubDir: string): string {
  const pkgPath = path.join(stubDir, 'package.json');
  const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8')) as {
    name?: string;
    dependencies?: Record<string, string>;
  };
  if (!pkg.name) throw new Error(`stub package.json missing 'name': ${pkgPath}`);
  return pkg.name;
}

function readStubDependencies(stubDir: string): Record<string, string> {
  const pkg = JSON.parse(
    fs.readFileSync(path.join(stubDir, 'package.json'), 'utf8')
  ) as { dependencies?: Record<string, string> };
  return pkg.dependencies ?? {};
}

/** Package dir under packages/ == the sanitized suffix of @carrick/<suffix>. */
function packageDirOf(packageName: string): string {
  const slash = packageName.lastIndexOf('/');
  return slash >= 0 ? packageName.slice(slash + 1) : packageName;
}

export interface AssembleOptions {
  stubs: CheckStubInput[];
  workspaceRoot?: string;
}

/** One file of a planned workspace, by its forward-slash path inside it. */
export type PlannedEntry = { data: Buffer } | { link: string };

/**
 * Everything a judge workspace holds before its install, as data: the stub
 * packages, the root manifests, the probes package and (once added) the
 * probes. A fresh workspace is this written out; a kept one is brought to it
 * (carrick#2883).
 */
export interface WorkspacePlan extends Omit<AssembledWorkspace, 'workspaceDir' | 'guard' | 'probesDir'> {
  files: Map<string, PlannedEntry>;
}

/**
 * Read a stub tree into `files` under `prefix`, as the stub copy writes it:
 * `node_modules` is left out wherever it sits on the source path, and a link
 * is kept as a link to the target the copy would give it.
 */
function planTree(from: string, prefix: string, files: Map<string, PlannedEntry>): void {
  if (from.split(path.sep).includes('node_modules')) return;
  for (const entry of fs.readdirSync(from, { withFileTypes: true })) {
    const source = path.join(from, entry.name);
    const rel = `${prefix}/${entry.name}`;
    if (entry.isDirectory()) planTree(source, rel, files);
    else if (entry.isSymbolicLink()) {
      if (entry.name === 'node_modules') continue;
      files.set(rel, { link: path.resolve(from, fs.readlinkSync(source)) });
    } else if (entry.isFile()) files.set(rel, { data: fs.readFileSync(source) });
  }
}

/** Plan the workspace for these stubs; no file is written. */
export function planWorkspace(stubs: CheckStubInput[]): WorkspacePlan {
  const files = new Map<string, PlannedEntry>();
  const text = (rel: string, content: string) => files.set(rel, { data: Buffer.from(content) });
  text('.npmrc', NPMRC);
  text('pnpm-workspace.yaml', 'packages:\n  - "packages/*"\n');

  const assembled: AssembledStub[] = [];
  const dependencySets: { dependencies: Record<string, string> }[] = [];
  const edgeSets: ResolutionEdges[] = [];
  const svcToPkg = new Map<string, string>();
  const dirToPkg = new Map<string, string>();
  const dirToSvc = new Map<string, string>();

  for (const stub of stubs) {
    const packageName = readStubPackageName(stub.stub_dir);
    const packageDir = packageDirOf(packageName);
    planTree(stub.stub_dir, `packages/${packageDir}`, files);
    assembled.push({ serviceName: stub.service_name, packageDir, packageName });
    dependencySets.push({ dependencies: readStubDependencies(stub.stub_dir) });
    edgeSets.push(readStubResolution(stub.stub_dir));
    svcToPkg.set(stub.service_name, packageName);
    dirToPkg.set(packageDir, packageName);
    dirToSvc.set(packageDir, stub.service_name);
  }

  // Root manifest carries the semver-dedupe overrides and the recorded edges.
  const dedupe = computeDedupeOverrides(dependencySets);
  const merged: Record<string, string> = {
    ...dedupe,
    ...computeResolutionOverrides(edgeSets, dedupe),
  };
  const overrides = Object.fromEntries(
    Object.keys(merged)
      .sort()
      .map((k) => [k, merged[k]])
  );
  text(
    'package.json',
    JSON.stringify(
      {
        name: 'carrick-check-workspace',
        version: '0.0.0',
        private: true,
        ...(Object.keys(overrides).length > 0 ? { pnpm: { overrides } } : {}),
      },
      null,
      2
    ) + '\n'
  );

  // Sorted, so the stubs' order on the request never changes the manifest a
  // kept workspace's install is keyed on (carrick#2883).
  const probeDeps: Record<string, string> = {};
  for (const name of assembled.map((s) => s.packageName).sort()) probeDeps[name] = 'workspace:*';
  const probesRel = `packages/${PROBES_PACKAGE}`;
  text(
    `${probesRel}/package.json`,
    JSON.stringify(
      {
        name: PROBES_PACKAGE,
        version: '0.0.0',
        private: true,
        dependencies: probeDeps,
      },
      null,
      2
    ) + '\n'
  );
  text(`${probesRel}/tsconfig.json`, CHECKER_TSCONFIG);

  return {
    files,
    probesRel,
    stubs: assembled,
    packageOf: (s) => {
      const pkg = svcToPkg.get(s);
      if (!pkg) throw new Error(`no stub for service '${s}'`);
      return pkg;
    },
    packageLabelOf: (dir) => dirToPkg.get(dir),
    serviceOfPackageDir: (dir) => dirToSvc.get(dir),
  };
}

/** Add the generated probe files to the plan's probes package. */
export function planProbes(plan: WorkspacePlan, plans: ProbePlan[]): void {
  for (const probe of plans) {
    plan.files.set(`${plan.probesRel}/probes/${probe.fileName}`, { data: Buffer.from(probe.source) });
  }
}

/**
 * The files `pnpm install` reads: equal keys install the same tree, so a kept
 * workspace whose key is unchanged skips the install (carrick#2883).
 */
export function installKeyOf(plan: WorkspacePlan): string {
  const hash = createHash('sha256');
  for (const rel of [...plan.files.keys()].sort()) {
    if (!/(^|\/)package\.json$/.test(rel) && rel !== '.npmrc' && rel !== 'pnpm-workspace.yaml') continue;
    const entry = plan.files.get(rel)!;
    hash.update(rel).update('\0').update('data' in entry ? entry.data : `link:${entry.link}`).update('\0');
  }
  return hash.digest('hex');
}

/** A digest of one planned entry, to tell a changed file from an unchanged one. */
export function entryDigest(entry: PlannedEntry): string {
  return 'data' in entry
    ? createHash('sha1').update(entry.data).digest('hex')
    : `link:${entry.link}`;
}

/** Write one planned entry, replacing what is there. */
export function writeEntry(guard: WriteGuard, dir: string, rel: string, entry: PlannedEntry): void {
  const target = path.join(dir, ...rel.split('/'));
  guard.mkdir(path.dirname(target));
  if ('data' in entry) {
    if (fs.lstatSync(target, { throwIfNoEntry: false })?.isSymbolicLink()) guard.unlink(target);
    guard.writeFile(target, entry.data);
  } else {
    if (fs.lstatSync(target, { throwIfNoEntry: false })) guard.remove(target);
    guard.symlink(entry.link, target);
  }
}

/** Write the plan into a fresh directory under `workspaceRoot` (the OS temp dir by default). */
export function writeWorkspace(plan: WorkspacePlan, workspaceRoot?: string): AssembledWorkspace {
  const { dir: workspaceDir, guard } = WriteGuard.scratch('carrick-check-v2-', workspaceRoot);
  for (const [rel, entry] of plan.files) writeEntry(guard, workspaceDir, rel, entry);
  const ws = withDir(plan, workspaceDir, guard);
  guard.mkdir(path.join(ws.probesDir, 'probes'));
  return ws;
}

/** The plan as a workspace at `workspaceDir`. */
export function withDir(plan: WorkspacePlan, workspaceDir: string, guard: WriteGuard): AssembledWorkspace {
  const { files: _files, ...layout } = plan;
  return { ...layout, workspaceDir, guard, probesDir: path.join(workspaceDir, ...plan.probesRel.split('/')) };
}

/** Create the scratch workspace and copy in the stub packages. */
export function assembleWorkspace(opts: AssembleOptions): AssembledWorkspace {
  return writeWorkspace(planWorkspace(opts.stubs), opts.workspaceRoot);
}

const NPMRC = [
  // Every stub gets its own node_modules: two versions of one dep coexist and
  // tsc resolves each stub's ref to its own copy (the isolation guarantee).
  'node-linker=isolated',
  // Stubs pin arbitrary library majors; peer conflicts must degrade to
  // unverifiable via the probe gates, never abort the whole install.
  'strict-peer-dependencies=false',
  // Determinism: install exactly the pinned closure, no implicit peer pull-in.
  'auto-install-peers=false',
  // Determinism: the scratch workspace has no committed lockfile. Direct deps
  // are exact-pinned by the stubs, and every transitive edge a stub recorded
  // from the scanned repo's installed tree is pinned by a parent-scoped
  // override in the root manifest. Only unrecorded edges resolve, and for
  // those prefer-offline resolves from the local store/metadata cache
  // whenever possible (byte-stable across runs on a host), and the explicit
  // resolution-mode pins pnpm's resolver behavior across pnpm versions.
  'prefer-offline=true',
  'resolution-mode=highest',
  '',
].join('\n');

// skipLibCheck:false is load-bearing (surfaces cross-stub declare-global
// TS2717 collisions so the poison rule can convert them to honest
// unverifiables). noUnusedLocals/Parameters:false keep TS6133/6196 off the
// gate/assignment lines. moduleResolution bundler accepts both node16
// `.js`-suffixed and extensionless specifiers in one program. The rest is
// what `sidecarCompilerOptions` sets on every other program (carrick#2019):
// `tsc` reads this file itself, so each value is written out, and a test
// holds the file to what that function would make of it.
export const CHECKER_TSCONFIG =
  JSON.stringify(
    {
      compilerOptions: {
        strict: true,
        exactOptionalPropertyTypes: false,
        skipLibCheck: false,
        noUnusedLocals: false,
        noUnusedParameters: false,
        moduleResolution: 'bundler',
        module: 'esnext',
        target: 'es2022',
        lib: ['es2022', 'dom', 'dom.iterable'],
        types: [],
        noEmit: true,
        forceConsistentCasingInFileNames: true,
        esModuleInterop: false,
        allowSyntheticDefaultImports: true,
        resolveJsonModule: true,
        alwaysStrict: true,
        noUncheckedSideEffectImports: false,
        libReplacement: true,
        stableTypeOrdering: true,
        ignoreDeprecations: '6.0',
      },
      include: ['probes/**/*.ts'],
    },
    null,
    2
  ) + '\n';
