/**
 * Exact-version dependency pins for the stub package, from two sources:
 *
 *  1. `installedVersions` — the repo's own node_modules, when installed.
 *     This is the version the repo actually resolves against, so it wins
 *     over the lockfile, and it covers repos whose lockfiles we do not
 *     parse at all (yarn classic v1, binary bun.lockb).
 *  2. `lockfileVersions` — parsed lockfile fallback for bare checkouts
 *     (npm v1/v2/v3, pnpm v6/v9, yarn-berry v2+, and bun's text bun.lock
 *     in scope).
 *
 * Pin selection prefers the DIRECT (top-level) dependency version: a package
 * present at multiple versions must pin to the version the emitted tree
 * resolves against — the hoisted/direct install — never to whichever nested
 * copy happens to appear first in the lockfile (npm sorts
 * `node_modules/a/node_modules/b` before `node_modules/b`, so first-match
 * picks a nested version the tree never sees). When a lockfile offers
 * multiple versions and no direct install can be identified, the package
 * stays UNPINNED (fail-closed abstain): a wrong pin fails or corrupts the
 * synthetic-workspace typecheck, no pin merely abstains.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';

/** package-lock.json (v2/v3 `packages` map, v1 `dependencies` fallback). */
function npmLockfileVersions(lockPath: string): Map<string, string> {
  const versions = new Map<string, string>();
  const nested = new Map<string, string>();
  try {
    const lock = JSON.parse(fs.readFileSync(lockPath, 'utf8'));
    if (lock.packages && typeof lock.packages === 'object') {
      for (const [key, entry] of Object.entries<{ version?: string }>(lock.packages)) {
        const idx = key.lastIndexOf('node_modules/');
        if (idx === -1 || !entry?.version) continue;
        const name = key.slice(idx + 'node_modules/'.length);
        // Direct dependency: the top-level install path, exactly
        // `node_modules/<name>` (idx === 0 means no nesting prefix).
        if (idx === 0) {
          if (!versions.has(name)) versions.set(name, entry.version);
        } else if (!nested.has(name)) {
          nested.set(name, entry.version);
        }
      }
      // Nested copies only pin packages with no top-level install at all.
      for (const [name, version] of nested) {
        if (!versions.has(name)) versions.set(name, version);
      }
    } else if (lock.dependencies && typeof lock.dependencies === 'object') {
      for (const [name, entry] of Object.entries<{ version?: string }>(lock.dependencies)) {
        if (entry?.version) versions.set(name, entry.version);
      }
    }
  } catch {
    // Unparseable lockfile: every external ends up in unpinned_externals.
  }
  return versions;
}

/** Strip a pnpm peer suffix / quoting from a resolved version value. */
function cleanPnpmVersion(raw: string): string {
  return raw.trim().replace(/^['"]|['"]$/g, '').split('(')[0].trim();
}

/**
 * pnpm-lock.yaml. Two tiers, both via a targeted line parse (no YAML dep):
 *
 *  1. `importers:` — every importer's dependencies/devDependencies/
 *     optionalDependencies with their RESOLVED `version:` values. These are
 *     the direct (top-level) installs and win.
 *  2. `packages:` — the flat closure. Keys look like `/zod@3.23.0:` (v6),
 *     `zod@3.23.0:` (v9), `/@scope/pkg@1.2.3(peer@x)…:`. Fallback for
 *     packages that are not a direct dependency of any importer; the
 *     packages map is name-sorted, so first-match here would otherwise pin
 *     the LOWEST version of a multi-version package.
 */
function pnpmLockfileVersions(lockPath: string): Map<string, string> {
  const direct = new Map<string, string>();
  const fallback = new Map<string, string>();
  try {
    const text = fs.readFileSync(lockPath, 'utf8');
    const lines = text.split('\n');

    let inPackages = false;
    let inImporters = false;
    let inDepsSection = false;
    let currentDep: string | undefined;

    for (const line of lines) {
      if (/^\S/.test(line)) {
        // New top-level key.
        inPackages = /^packages:\s*$/.test(line);
        inImporters = /^importers:\s*$/.test(line);
        inDepsSection = false;
        currentDep = undefined;
        continue;
      }

      if (inImporters) {
        // `  <importer>:` (2-space) resets; `    dependencies:` (4-space)
        // opens a deps section; `      <name>:` (6-space) names a dep;
        // `        version: <v>` (8-space) resolves it.
        if (/^ {2}\S/.test(line)) {
          inDepsSection = false;
          currentDep = undefined;
        } else if (/^ {4}\S/.test(line)) {
          inDepsSection = /^ {4}(dependencies|devDependencies|optionalDependencies):\s*$/.test(
            line
          );
          currentDep = undefined;
        } else if (inDepsSection && /^ {6}\S/.test(line)) {
          const m = /^ {6}['"]?([^'":]+)['"]?:\s*(\S.*)?$/.exec(line);
          currentDep = m?.[1];
          // Old inline form: `      zod: 3.23.8`.
          if (m?.[1] && m[2] && !direct.has(m[1])) {
            const v = cleanPnpmVersion(m[2]);
            if (/^\d/.test(v)) direct.set(m[1], v);
          }
        } else if (inDepsSection && currentDep && /^ {8}version:/.test(line)) {
          const v = cleanPnpmVersion(line.slice(line.indexOf(':') + 1));
          if (v && !direct.has(currentDep) && /^\d/.test(v)) direct.set(currentDep, v);
          currentDep = undefined;
        }
        continue;
      }

      if (inPackages) {
        // Two-space-indented keys only (package entries), e.g.
        //   /zod@3.23.0: | 'zod@3.23.0': | /@scope/pkg@1.2.3(peer@1.0.0):
        const m = /^ {2}['"]?\/?((?:@[^/@'"]+\/)?[^/@'"]+)@([^('":]+)/.exec(line);
        if (!m) continue;
        const [, name, version] = m;
        if (!fallback.has(name)) fallback.set(name, version.trim());
      }
    }
  } catch {
    // Unparseable lockfile: every external ends up in unpinned_externals.
  }

  const versions = new Map<string, string>(direct);
  for (const [name, version] of fallback) {
    if (!versions.has(name)) versions.set(name, version);
  }
  return versions;
}

/**
 * Published-semver gate for node_modules pins. Rejects anything a registry
 * install could not satisfy: workspace/link protocol leakage
 * (`workspace:*`, `link:...`) and yarn-berry's `0.0.0-use.local` local
 * sentinel. A rejected version means "unpinned" (fail-closed abstain
 * downstream), never a pin that would fail the synthetic-workspace install.
 */
export function isPublishedSemver(version: string): boolean {
  return (
    /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version) &&
    !version.startsWith('0.0.0-use.')
  );
}

/**
 * Direct-dependency ranges declared by the manifest sitting next to a
 * lockfile: the workspace root's dependencies/devDependencies/
 * optionalDependencies. Used to disambiguate multi-version packages in
 * lockfiles that key entries by requested range (yarn-berry).
 */
function rootPackageJsonRanges(pkgJsonPath: string): Map<string, string> {
  const ranges = new Map<string, string>();
  try {
    const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, 'utf8'));
    for (const section of ['dependencies', 'devDependencies', 'optionalDependencies']) {
      const deps = pkg?.[section];
      if (!deps || typeof deps !== 'object') continue;
      for (const [name, range] of Object.entries(deps)) {
        if (typeof range === 'string' && !ranges.has(name)) ranges.set(name, range);
      }
    }
  } catch {
    // No/unreadable root manifest: multi-version packages abstain.
  }
  return ranges;
}

/**
 * yarn-berry (v2+) yarn.lock. Detected by its `__metadata:` block; a yarn
 * CLASSIC (v1) lockfile has no such block and a different entry syntax, so
 * it bails to an empty map here (classic is not parsed — `installedVersions`
 * covers installed classic checkouts).
 *
 * Entries key a comma-separated descriptor set on the resolved package:
 *
 *   "ms@npm:2.0.0":            →   version: 2.0.0
 *   "ms@npm:^2.1.3":           →   version: 2.1.3
 *   "app@workspace:.":         →   version: 0.0.0-use.local
 *
 * Only `npm:` protocol descriptors are considered (`workspace:`, `patch:`,
 * `portal:`, `link:`, git — none registry-installable as the bare name).
 * npm-alias descriptors (`foo@npm:bar@^1.0.0`) resolve a DIFFERENT package
 * than the install name, so they never pin either.
 *
 * Multi-version selection: the lockfile keys by requested range, not by
 * install position, so the direct install is identified through the
 * workspace-root package.json — the entry whose descriptor set contains
 * `<name>@npm:<root-declared-range>` wins. Multiple versions and no direct
 * match → unpinned (abstain).
 */
function yarnBerryLockfileVersions(lockPath: string): Map<string, string> {
  const versions = new Map<string, string>();
  try {
    const text = fs.readFileSync(lockPath, 'utf8');
    if (!/^__metadata:/m.test(text)) return versions;

    // name → (requested range → resolved version), npm: descriptors only.
    const byName = new Map<string, Map<string, string>>();
    let current: Array<{ name: string; range: string }> = [];
    for (const line of text.split('\n')) {
      if (line.startsWith('#')) continue;
      const keyMatch = /^(\S.*):\s*$/.exec(line);
      if (keyMatch) {
        // New top-level entry: a descriptor set (or `__metadata`).
        current = [];
        if (keyMatch[1] === '__metadata') continue;
        for (const rawDesc of keyMatch[1].split(',')) {
          const desc = rawDesc.trim().replace(/^['"]|['"]$/g, '');
          const at = desc.indexOf('@', 1); // 1: skip a scope's leading @
          if (at <= 0) continue;
          const name = desc.slice(0, at);
          const protocolAndRange = desc.slice(at + 1);
          if (!protocolAndRange.startsWith('npm:')) continue;
          const range = protocolAndRange.slice('npm:'.length);
          if (range.includes('@')) continue; // npm alias: foo@npm:bar@^1.0.0
          current.push({ name, range });
        }
        continue;
      }
      const versionMatch = /^ {2}version:\s*(.+?)\s*$/.exec(line);
      if (versionMatch && current.length > 0) {
        const version = versionMatch[1].replace(/^['"]|['"]$/g, '');
        for (const { name, range } of current) {
          let ranges = byName.get(name);
          if (!ranges) byName.set(name, (ranges = new Map()));
          ranges.set(range, version);
        }
        current = [];
      }
    }

    const rootRanges = rootPackageJsonRanges(
      path.join(path.dirname(lockPath), 'package.json')
    );
    for (const [name, ranges] of byName) {
      const distinct = new Set(ranges.values());
      let resolved: string | undefined;
      if (distinct.size === 1) {
        resolved = distinct.values().next().value;
      } else {
        const declared = rootRanges.get(name);
        if (declared !== undefined) {
          const expected = declared.startsWith('npm:')
            ? declared.slice('npm:'.length)
            : declared;
          resolved = ranges.get(expected);
        }
        // No direct match among multiple versions: abstain.
      }
      if (resolved !== undefined && isPublishedSemver(resolved)) {
        versions.set(name, resolved);
      }
    }
  } catch {
    // Unparseable lockfile: every external ends up in unpinned_externals.
  }
  return versions;
}

/**
 * Strip JSONC affordances (line/block comments, trailing commas) so bun's
 * text lockfile can be JSON.parsed. String-aware: `//` inside a string
 * (registry URLs) is content, not a comment.
 */
function stripJsonc(text: string): string {
  let noComments = '';
  let inString = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inString) {
      noComments += c;
      if (c === '\\') {
        noComments += text[i + 1] ?? '';
        i++;
      } else if (c === '"') {
        inString = false;
      }
      continue;
    }
    if (c === '"') {
      inString = true;
      noComments += c;
    } else if (c === '/' && text[i + 1] === '/') {
      while (i < text.length && text[i] !== '\n') i++;
      i--;
    } else if (c === '/' && text[i + 1] === '*') {
      i += 2;
      while (i < text.length && !(text[i] === '*' && text[i + 1] === '/')) i++;
      i++;
    } else {
      noComments += c;
    }
  }

  let out = '';
  inString = false;
  for (let i = 0; i < noComments.length; i++) {
    const c = noComments[i];
    if (inString) {
      out += c;
      if (c === '\\') {
        out += noComments[i + 1] ?? '';
        i++;
      } else if (c === '"') {
        inString = false;
      }
      continue;
    }
    if (c === '"') {
      inString = true;
    } else if (c === ',') {
      let j = i + 1;
      while (j < noComments.length && /\s/.test(noComments[j])) j++;
      if (noComments[j] === '}' || noComments[j] === ']') continue; // trailing
    }
    out += c;
  }
  return out;
}

/**
 * bun's TEXT bun.lock (JSONC). The binary bun.lockb is out of scope. The
 * `packages` map is keyed by hoisted install path — `"ms"` is the top-level
 * install, `"debug/ms"` a copy nested under debug — with the resolution as
 * the first tuple element (`"ms@2.1.3"`, `"@probe/member@workspace:…"`).
 *
 * Mirrors the npm parser's direct-first rule: the top-level key (key ===
 * package name) is what the emitted tree resolves against and wins. A
 * top-level entry that fails the published-semver gate (workspace member,
 * git, tarball) blocks nested fallback entirely — the tree resolves to the
 * unpinnable thing, so pinning a nested registry copy would be wrong.
 * Nested-only packages pin solely when every nested copy agrees on one
 * version; several distinct nested versions → unpinned (abstain). Alias
 * installs (key name ≠ resolved package name) never pin.
 */
function bunLockfileVersions(lockPath: string): Map<string, string> {
  const versions = new Map<string, string>();
  try {
    const lock = JSON.parse(stripJsonc(fs.readFileSync(lockPath, 'utf8')));
    const packages = lock?.packages;
    if (!packages || typeof packages !== 'object') return versions;

    const directNames = new Set<string>();
    const nested = new Map<string, Set<string>>();
    for (const [key, entry] of Object.entries(packages)) {
      if (!Array.isArray(entry) || typeof entry[0] !== 'string') continue;
      const spec = entry[0]; // "<name>@<resolution>"
      const at = spec.indexOf('@', 1); // 1: skip a scope's leading @
      if (at <= 0) continue;
      const name = spec.slice(0, at);
      const resolved = spec.slice(at + 1);
      if (key === name) {
        directNames.add(name);
        if (isPublishedSemver(resolved)) versions.set(name, resolved);
      } else if (key.endsWith(`/${name}`)) {
        let copies = nested.get(name);
        if (!copies) nested.set(name, (copies = new Set()));
        copies.add(resolved);
      }
    }
    for (const [name, copies] of nested) {
      if (directNames.has(name) || copies.size !== 1) continue;
      const version = copies.values().next().value;
      if (version !== undefined && isPublishedSemver(version)) versions.set(name, version);
    }
  } catch {
    // Unparseable lockfile: every external ends up in unpinned_externals.
  }
  return versions;
}

/**
 * Resolve exact versions from the repo's installed node_modules by reading
 * `node_modules/<pkg>/package.json` for each referenced external. On an
 * installed checkout this is ground truth — the versions the repo actually
 * runs against — and it needs no lockfile parser, which is what covers the
 * formats without one (yarn classic v1, binary bun.lockb).
 *
 * Workspace-member guard (fail-closed): package managers link workspace
 * siblings into node_modules as symlinks (`node_modules/@ws/member ->
 * ../../packages/member`). Those carry unpublished versions; pinning one
 * would make the whole synthetic-workspace install fail at check time. So a
 * pin is taken only when the entry's realpath still lives under a
 * node_modules directory inside the repo. pnpm's own
 * `node_modules/<pkg> -> node_modules/.pnpm/<pkg>@<v>/node_modules/<pkg>`
 * links pass that test and stay pinnable; realpaths inside the repo but
 * outside every node_modules (workspace members) and realpaths outside the
 * repo entirely (npm/yarn link to a dev checkout) are skipped — the
 * external stays unpinned unless the lockfile supplies a version.
 */
export function installedVersions(
  repoRoot: string,
  names: Iterable<string>
): Map<string, string> {
  const versions = new Map<string, string>();
  const nodeModules = path.join(path.resolve(repoRoot), 'node_modules');
  let realRepoRoot: string;
  try {
    realRepoRoot = fs.realpathSync(path.resolve(repoRoot));
  } catch {
    return versions;
  }
  for (const name of names) {
    // Package names come from bare import specifiers; anything with empty
    // or dot segments cannot be a node_modules entry (and must not escape).
    const segments = name.split('/');
    if (segments.some((s) => s === '' || s === '.' || s === '..')) continue;
    const pkgDir = path.join(nodeModules, ...segments);
    try {
      const real = fs.realpathSync(pkgDir);
      const rel = path.relative(realRepoRoot, real);
      const inRepo = rel !== '' && !rel.startsWith('..') && !path.isAbsolute(rel);
      if (!inRepo || !rel.split(path.sep).includes('node_modules')) continue;
      const manifest = JSON.parse(
        fs.readFileSync(path.join(pkgDir, 'package.json'), 'utf8')
      );
      if (typeof manifest?.version === 'string' && isPublishedSemver(manifest.version)) {
        versions.set(name, manifest.version);
      }
    } catch {
      // Missing dir, dangling symlink, unreadable or corrupt package.json:
      // the external stays unpinned here (lockfile fallback or abstain).
    }
  }
  return versions;
}

/**
 * Resolve exact versions for the repo. Walks up from repoRoot so monorepo
 * members with a hoisted root lockfile still pin (nearest lockfile wins;
 * package-lock.json > pnpm-lock.yaml > yarn.lock > bun.lock at the same
 * level).
 */
export function lockfileVersions(repoRoot: string): Map<string, string> {
  let dir = path.resolve(repoRoot);
  for (;;) {
    const npmLock = path.join(dir, 'package-lock.json');
    if (fs.existsSync(npmLock)) return npmLockfileVersions(npmLock);
    const pnpmLock = path.join(dir, 'pnpm-lock.yaml');
    if (fs.existsSync(pnpmLock)) return pnpmLockfileVersions(pnpmLock);
    const yarnLock = path.join(dir, 'yarn.lock');
    if (fs.existsSync(yarnLock)) return yarnBerryLockfileVersions(yarnLock);
    const bunLock = path.join(dir, 'bun.lock');
    if (fs.existsSync(bunLock)) return bunLockfileVersions(bunLock);
    const parent = path.dirname(dir);
    if (parent === dir) return new Map();
    dir = parent;
  }
}
