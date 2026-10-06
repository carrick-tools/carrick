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

import * as fs from 'node:fs';
import * as path from 'node:path';
import type { CheckStubInput } from './api.js';
import type { ProbePlan } from './check-probe.js';
import { WriteGuard } from './guarded-fs.js';

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

/** Create the scratch workspace and copy in the stub packages. */
export function assembleWorkspace(opts: AssembleOptions): AssembledWorkspace {
  // A fresh directory under the caller's root (the OS temp dir by default).
  const { dir: workspaceDir, guard } = WriteGuard.scratch('carrick-check-v2-', opts.workspaceRoot);
  const packagesDir = path.join(workspaceDir, 'packages');
  guard.mkdir(packagesDir);

  guard.writeFile(path.join(workspaceDir, '.npmrc'), NPMRC);
  guard.writeFile(
    path.join(workspaceDir, 'pnpm-workspace.yaml'),
    'packages:\n  - "packages/*"\n'
  );

  const assembled: AssembledStub[] = [];
  const dependencySets: { dependencies: Record<string, string> }[] = [];
  const svcToPkg = new Map<string, string>();
  const dirToPkg = new Map<string, string>();
  const dirToSvc = new Map<string, string>();

  for (const stub of opts.stubs) {
    const packageName = readStubPackageName(stub.stub_dir);
    const packageDir = packageDirOf(packageName);
    const dest = path.join(packagesDir, packageDir);
    guard.copyTree(stub.stub_dir, dest, (src) => !src.split(path.sep).includes('node_modules'));
    assembled.push({ serviceName: stub.service_name, packageDir, packageName });
    dependencySets.push({ dependencies: readStubDependencies(stub.stub_dir) });
    svcToPkg.set(stub.service_name, packageName);
    dirToPkg.set(packageDir, packageName);
    dirToSvc.set(packageDir, stub.service_name);
  }

  // Root manifest carries the semver-dedupe overrides.
  const overrides = computeDedupeOverrides(dependencySets);
  guard.writeFile(
    path.join(workspaceDir, 'package.json'),
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

  const probesDir = path.join(packagesDir, PROBES_PACKAGE);
  guard.mkdir(path.join(probesDir, 'probes'));
  const probeDeps: Record<string, string> = {};
  for (const stub of assembled) probeDeps[stub.packageName] = 'workspace:*';
  guard.writeFile(
    path.join(probesDir, 'package.json'),
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
  guard.writeFile(path.join(probesDir, 'tsconfig.json'), CHECKER_TSCONFIG);

  return {
    workspaceDir,
    guard,
    probesDir,
    probesRel: `packages/${PROBES_PACKAGE}`,
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

/** Write the generated probe files into the assembled probes package. */
export function writeProbes(ws: AssembledWorkspace, plans: ProbePlan[]): void {
  for (const plan of plans) {
    ws.guard.writeFile(path.join(ws.probesDir, 'probes', plan.fileName), plan.source);
  }
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
  // Determinism: the scratch workspace has no committed lockfile, so the
  // transitive closure would otherwise be a pure function of live registry
  // state. prefer-offline resolves from the local store/metadata cache
  // whenever possible (byte-stable across runs on a host), and the explicit
  // resolution-mode pins pnpm's resolver behavior across pnpm versions.
  // Direct deps are exact-pinned by the stubs; only transitives resolve.
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
