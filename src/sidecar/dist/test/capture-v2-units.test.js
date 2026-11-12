/**
 * Unit tests for the capture bundle's deterministic helpers: lockfile pin
 * parsing (npm v3, npm v1, pnpm v6/v9, yarn-berry v2+, text bun.lock,
 * walk-up/preference order) and specifier matching/rewriting (the
 * paths-rewrite building blocks).
 */
import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { lockfileVersions } from '../src/capture/lockfile.js';
import { collectSpecifiers, matchPathsPattern, packageNameOf, rewriteSpecifiers, } from '../src/capture/specifiers.js';
function tempDir() {
    return fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-units-'));
}
describe('lockfileVersions', () => {
    it('parses npm lockfile v3 packages map, direct entry wins', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'package-lock.json'), JSON.stringify({
            lockfileVersion: 3,
            packages: {
                '': {},
                'node_modules/zod': { version: '3.23.0' },
                'node_modules/@scope/pkg': { version: '2.0.1' },
                'node_modules/other/node_modules/zod': { version: '4.0.0' },
            },
        }));
        const versions = lockfileVersions(dir);
        assert.strictEqual(versions.get('zod'), '3.23.0');
        assert.strictEqual(versions.get('@scope/pkg'), '2.0.1');
    });
    it('npm: prefers the direct dependency when a nested copy sorts first', () => {
        // npm sorts `node_modules/a/node_modules/b` BEFORE `node_modules/b`, so
        // first-match-wins pinned the nested version the emitted tree never
        // resolves against (adversarial-review finding 4).
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'package-lock.json'), JSON.stringify({
            lockfileVersion: 3,
            packages: {
                '': {},
                'node_modules/aaa': { version: '1.0.0' },
                'node_modules/aaa/node_modules/zod': { version: '2.9.9' },
                'node_modules/zod': { version: '3.23.0' },
                // Nested-only package: the nested copy is the only install.
                'node_modules/aaa/node_modules/nested-only': { version: '5.5.5' },
            },
        }));
        const versions = lockfileVersions(dir);
        assert.strictEqual(versions.get('zod'), '3.23.0');
        assert.strictEqual(versions.get('nested-only'), '5.5.5');
    });
    it('pnpm: prefers the importer-resolved (direct) version over the packages map', () => {
        // The pnpm packages map is name-sorted, so a multi-version package's
        // first entry is its LOWEST version; the importers section carries the
        // resolved direct-dependency version (adversarial-review finding 4).
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'pnpm-lock.yaml'), [
            'lockfileVersion: 9.0',
            '',
            'importers:',
            '  .:',
            '    dependencies:',
            '      zod:',
            "        specifier: ^3.24.0",
            "        version: 3.24.1",
            '  packages/member:',
            '    dependencies:',
            '      redis:',
            "        specifier: ^4.6.0",
            "        version: 4.6.7(peer@1.0.0)",
            '',
            'packages:',
            '',
            // Lower version of a transitive copy sorts first.
            "  zod@3.22.0:",
            "    resolution: {integrity: sha512-a}",
            '',
            "  zod@3.24.1:",
            "    resolution: {integrity: sha512-b}",
            '',
            "  redis@4.6.7:",
            "    resolution: {integrity: sha512-c}",
            '',
            "  transitive-only@1.2.3:",
            "    resolution: {integrity: sha512-d}",
            '',
        ].join('\n'));
        const versions = lockfileVersions(dir);
        assert.strictEqual(versions.get('zod'), '3.24.1');
        // Peer suffix stripped from the importer-resolved version.
        assert.strictEqual(versions.get('redis'), '4.6.7');
        // Non-direct packages still pin from the packages map.
        assert.strictEqual(versions.get('transitive-only'), '1.2.3');
    });
    it('parses npm lockfile v1 dependencies fallback', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'package-lock.json'), JSON.stringify({
            lockfileVersion: 1,
            dependencies: { express: { version: '4.18.2' } },
        }));
        assert.strictEqual(lockfileVersions(dir).get('express'), '4.18.2');
    });
    it('parses pnpm-lock.yaml v6 and v9 style package keys', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'pnpm-lock.yaml'), [
            'lockfileVersion: 9.0',
            '',
            'importers:',
            '  .:',
            '    dependencies:',
            '      zod:',
            "        specifier: ^3.23.0",
            "        version: 3.23.0",
            '',
            'packages:',
            '',
            "  zod@3.23.0:",
            "    resolution: {integrity: sha512-x}",
            '',
            "  '@scope/pkg@2.4.0(peerdep@1.0.0)':",
            "    resolution: {integrity: sha512-y}",
            '',
            "  /legacy-style@1.1.1:",
            "    resolution: {integrity: sha512-z}",
            '',
        ].join('\n'));
        const versions = lockfileVersions(dir);
        assert.strictEqual(versions.get('zod'), '3.23.0');
        assert.strictEqual(versions.get('@scope/pkg'), '2.4.0');
        assert.strictEqual(versions.get('legacy-style'), '1.1.1');
    });
    it('walks up to a hoisted monorepo lockfile', () => {
        const root = tempDir();
        const member = path.join(root, 'packages', 'svc');
        fs.mkdirSync(member, { recursive: true });
        fs.writeFileSync(path.join(root, 'package-lock.json'), JSON.stringify({
            lockfileVersion: 3,
            packages: { 'node_modules/redis': { version: '4.6.0' } },
        }));
        assert.strictEqual(lockfileVersions(member).get('redis'), '4.6.0');
    });
    it('returns empty for an unparseable lockfile (everything unpinned)', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'package-lock.json'), '{nope');
        assert.strictEqual(lockfileVersions(dir).size, 0);
    });
});
/**
 * Berry fixture body modeled on a real yarn 4 lockfile (`__metadata:
 * version: 10`): quoted descriptor-set keys, unquoted `version:` values,
 * a workspace entry at `0.0.0-use.local`, and a builtin-patch entry
 * alongside the plain `npm:` entry it patches.
 */
function writeBerryLock(dir, entries) {
    fs.writeFileSync(path.join(dir, 'yarn.lock'), [
        '# This file is generated by running "yarn install" inside your project.',
        '# Manual changes might be lost - proceed with caution!',
        '',
        '__metadata:',
        '  version: 10',
        '  cacheKey: 10c0',
        '',
        ...entries,
    ].join('\n'));
}
describe('lockfileVersions (yarn-berry)', () => {
    it('pins single-version packages, including scoped and unquoted-key entries', () => {
        const dir = tempDir();
        writeBerryLock(dir, [
            '"debug@npm:2.6.9":',
            '  version: 2.6.9',
            '  resolution: "debug@npm:2.6.9"',
            '  dependencies:',
            '    ms: "npm:2.0.0"',
            '  languageName: node',
            '  linkType: hard',
            '',
            // yarn 3 writes plain unscoped keys unquoted.
            'acorn@npm:^8.9.0:',
            '  version: 8.10.0',
            '  resolution: "acorn@npm:8.10.0"',
            '  languageName: node',
            '  linkType: hard',
            '',
            '"@scope/pkg@npm:^2.0.0, @scope/pkg@npm:^2.1.0":',
            '  version: 2.4.0',
            '  resolution: "@scope/pkg@npm:2.4.0"',
            '  languageName: node',
            '  linkType: hard',
            '',
        ]);
        const versions = lockfileVersions(dir);
        assert.strictEqual(versions.get('debug'), '2.6.9');
        assert.strictEqual(versions.get('acorn'), '8.10.0');
        assert.strictEqual(versions.get('@scope/pkg'), '2.4.0');
    });
    it('multi-version: pins the root package.json (direct) range, not first match', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ dependencies: { ms: '^2.1.3' } }));
        writeBerryLock(dir, [
            // The transitive 2.0.0 entry sorts before the direct ^2.1.3 entry.
            '"ms@npm:2.0.0":',
            '  version: 2.0.0',
            '  resolution: "ms@npm:2.0.0"',
            '  languageName: node',
            '  linkType: hard',
            '',
            '"ms@npm:^2.1.3":',
            '  version: 2.1.3',
            '  resolution: "ms@npm:2.1.3"',
            '  languageName: node',
            '  linkType: hard',
            '',
        ]);
        assert.strictEqual(lockfileVersions(dir).get('ms'), '2.1.3');
    });
    it('multi-version with no direct match abstains (unpinned, fail-closed)', () => {
        const dir = tempDir();
        // Root manifest does not declare ms at all: no safe pick exists.
        fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ dependencies: { unrelated: '^1.0.0' } }));
        writeBerryLock(dir, [
            '"ms@npm:2.0.0":',
            '  version: 2.0.0',
            '  resolution: "ms@npm:2.0.0"',
            '  languageName: node',
            '  linkType: hard',
            '',
            '"ms@npm:^2.1.3":',
            '  version: 2.1.3',
            '  resolution: "ms@npm:2.1.3"',
            '  languageName: node',
            '  linkType: hard',
            '',
        ]);
        assert.strictEqual(lockfileVersions(dir).get('ms'), undefined);
    });
    it('multi-version with a missing root package.json abstains', () => {
        const dir = tempDir();
        writeBerryLock(dir, [
            '"ms@npm:2.0.0":',
            '  version: 2.0.0',
            '  languageName: node',
            '',
            '"ms@npm:^2.1.3":',
            '  version: 2.1.3',
            '  languageName: node',
            '',
        ]);
        assert.strictEqual(lockfileVersions(dir).get('ms'), undefined);
    });
    it('never pins workspace, patch-only, or npm-alias descriptors', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({
            dependencies: { '@probe/member': 'workspace:*', myalias: 'npm:ms@^2.1.3' },
            devDependencies: { typescript: '~5.4.0' },
        }));
        writeBerryLock(dir, [
            '"@probe/member@workspace:*, @probe/member@workspace:packages/member":',
            '  version: 0.0.0-use.local',
            '  resolution: "@probe/member@workspace:packages/member"',
            '  languageName: unknown',
            '  linkType: soft',
            '',
            // npm alias: installs under the name myalias but resolves package ms;
            // a registry install of myalias@2.1.3 would be a different package.
            '"myalias@npm:ms@^2.1.3":',
            '  version: 2.1.3',
            '  resolution: "ms@npm:2.1.3"',
            '  languageName: node',
            '  linkType: hard',
            '',
            // Builtin patch alongside the plain npm entry it patches: the patch
            // descriptor is skipped, the npm one still pins.
            '"typescript@npm:~5.4.0":',
            '  version: 5.4.5',
            '  resolution: "typescript@npm:5.4.5"',
            '  languageName: node',
            '  linkType: hard',
            '',
            '"typescript@patch:typescript@npm%3A~5.4.0#optional!builtin<compat/typescript>":',
            '  version: 5.4.5',
            '  resolution: "typescript@patch:typescript@npm%3A5.4.5#optional!builtin<compat/typescript>::version=5.4.5&hash=5adc0c"',
            '  languageName: node',
            '  linkType: hard',
            '',
            '"root-ws@workspace:.":',
            '  version: 0.0.0-use.local',
            '  resolution: "root-ws@workspace:."',
            '  languageName: unknown',
            '  linkType: soft',
            '',
        ]);
        const versions = lockfileVersions(dir);
        assert.strictEqual(versions.get('@probe/member'), undefined);
        assert.strictEqual(versions.get('myalias'), undefined);
        assert.strictEqual(versions.get('root-ws'), undefined);
        assert.strictEqual(versions.get('typescript'), '5.4.5');
    });
    it('bails cleanly on a yarn CLASSIC (v1) lockfile (no __metadata block)', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'yarn.lock'), [
            '# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.',
            '# yarn lockfile v1',
            '',
            '',
            'ms@^2.1.3:',
            '  version "2.1.3"',
            '  resolved "https://registry.yarnpkg.com/ms/-/ms-2.1.3.tgz#574c8138ce1d2b5861f0b44579dbadd60c6615b2"',
            '  integrity sha512-6FlzubTLZG3J2a/NVCAleEhjzq5oxgHyaCU9yYXvcLsvoVaHJq/s5xXI6/XXP6tz7R9xAOtHnSO/tXtF3WRTlA==',
            '',
        ].join('\n'));
        assert.strictEqual(lockfileVersions(dir).size, 0);
    });
});
describe('lockfileVersions (bun)', () => {
    /** Modeled on real `bun install` output (lockfileVersion 1): JSONC with
     *  trailing commas, packages keyed by hoisted install path. */
    const bunLock = [
        '{',
        '  // bun tolerates comments in its lockfile',
        '  "lockfileVersion": 1,',
        '  "configVersion": 1,',
        '  "workspaces": {',
        '    "": {',
        '      "name": "bun-probe",',
        '      "dependencies": {',
        '        "@probe/member": "workspace:*",',
        '        "debug": "2.6.9",',
        '        "ms": "^2.1.3",',
        '      },',
        '    },',
        '    "packages/member": {',
        '      "name": "@probe/member",',
        '      "version": "0.0.1",',
        '      "dependencies": {',
        '        "ms": "2.0.0",',
        '      },',
        '    },',
        '  },',
        '  "packages": {',
        '    "@probe/member": ["@probe/member@workspace:packages/member"],',
        '',
        '    "debug": ["debug@2.6.9", "https://registry.npmjs.org/debug/-/debug-2.6.9.tgz", { "dependencies": { "ms": "2.0.0" } }, "sha512-bC7Elrd"],',
        '',
        '    "ms": ["ms@2.1.3", "", {}, "sha512-6Flzub"],',
        '',
        '    "@probe/member/ms": ["ms@2.0.0", "", {}, "sha512-Tpp60P"],',
        '',
        '    "debug/ms": ["ms@2.0.0", "", {}, "sha512-Tpp60P"],',
        '  }',
        '}',
    ].join('\n');
    it('pins top-level installs over nested copies; workspace members never pin', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'bun.lock'), bunLock);
        const versions = lockfileVersions(dir);
        assert.strictEqual(versions.get('ms'), '2.1.3');
        assert.strictEqual(versions.get('debug'), '2.6.9');
        assert.strictEqual(versions.get('@probe/member'), undefined);
    });
    it('nested-only copies pin only when unambiguous', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'bun.lock'), [
            '{',
            '  "lockfileVersion": 1,',
            '  "workspaces": { "": { "name": "app" } },',
            '  "packages": {',
            '    "a": ["a@1.0.0", "", {}, "sha512-a"],',
            '    "b": ["b@1.0.0", "", {}, "sha512-b"],',
            // Single nested version: safe to pin.
            '    "a/agreed": ["agreed@3.0.0", "", {}, "sha512-c"],',
            '    "b/agreed": ["agreed@3.0.0", "", {}, "sha512-c"],',
            // Two distinct nested versions and no top-level install: abstain.
            '    "a/conflicted": ["conflicted@1.1.1", "", {}, "sha512-d"],',
            '    "b/conflicted": ["conflicted@2.2.2", "", {}, "sha512-e"],',
            // Top-level entry is a workspace member: blocks nested fallback.
            '    "wsdep": ["wsdep@workspace:packages/wsdep"],',
            '    "a/wsdep": ["wsdep@9.9.9", "", {}, "sha512-f"],',
            '  }',
            '}',
        ].join('\n'));
        const versions = lockfileVersions(dir);
        assert.strictEqual(versions.get('agreed'), '3.0.0');
        assert.strictEqual(versions.get('conflicted'), undefined);
        assert.strictEqual(versions.get('wsdep'), undefined);
    });
    it('returns empty for an unparseable bun.lock', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'bun.lock'), '{nope');
        assert.strictEqual(lockfileVersions(dir).size, 0);
    });
});
describe('lockfileVersions (walk-up and preference order)', () => {
    it('walks up to a hoisted berry or bun lockfile', () => {
        const yarnRoot = tempDir();
        const yarnMember = path.join(yarnRoot, 'packages', 'svc');
        fs.mkdirSync(yarnMember, { recursive: true });
        writeBerryLock(yarnRoot, [
            '"redis@npm:^4.6.0":',
            '  version: 4.6.7',
            '  languageName: node',
            '',
        ]);
        assert.strictEqual(lockfileVersions(yarnMember).get('redis'), '4.6.7');
        const bunRoot = tempDir();
        const bunMember = path.join(bunRoot, 'packages', 'svc');
        fs.mkdirSync(bunMember, { recursive: true });
        fs.writeFileSync(path.join(bunRoot, 'bun.lock'), '{ "lockfileVersion": 1, "packages": { "redis": ["redis@4.6.7", "", {}, "sha512-x"], } }');
        assert.strictEqual(lockfileVersions(bunMember).get('redis'), '4.6.7');
    });
    it('prefers pnpm-lock.yaml over yarn.lock, and yarn.lock over bun.lock', () => {
        const dir = tempDir();
        fs.writeFileSync(path.join(dir, 'pnpm-lock.yaml'), ['lockfileVersion: 9.0', '', 'packages:', '', '  zod@3.23.0:', '    resolution: {integrity: sha512-x}', ''].join('\n'));
        writeBerryLock(dir, [
            '"zod@npm:^3.24.0":',
            '  version: 3.24.1',
            '  languageName: node',
            '',
        ]);
        fs.writeFileSync(path.join(dir, 'bun.lock'), '{ "lockfileVersion": 1, "packages": { "zod": ["zod@3.25.0", "", {}, "sha512-y"] } }');
        // pnpm wins while present…
        assert.strictEqual(lockfileVersions(dir).get('zod'), '3.23.0');
        // …then yarn.lock, then bun.lock.
        fs.rmSync(path.join(dir, 'pnpm-lock.yaml'));
        assert.strictEqual(lockfileVersions(dir).get('zod'), '3.24.1');
        fs.rmSync(path.join(dir, 'yarn.lock'));
        assert.strictEqual(lockfileVersions(dir).get('zod'), '3.25.0');
    });
});
describe('specifier helpers', () => {
    it('collects from-imports, bare imports, and import() type references', () => {
        const text = [
            "import { a } from './rel';",
            "import 'side-effect';",
            'export declare const x: import("zod").ZodType<string>;',
        ].join('\n');
        const specs = collectSpecifiers(text);
        assert.ok(specs.has('./rel'));
        assert.ok(specs.has('side-effect'));
        assert.ok(specs.has('zod'));
    });
    it('extracts package names from deep and scoped specifiers', () => {
        assert.strictEqual(packageNameOf('zod'), 'zod');
        assert.strictEqual(packageNameOf('zod/lib/types'), 'zod');
        assert.strictEqual(packageNameOf('@scope/pkg/sub/deep'), '@scope/pkg');
    });
    it('matches star and exact paths patterns', () => {
        const star = { pattern: '@app/*', prefix: '@app/', suffix: '', targets: [] };
        assert.strictEqual(matchPathsPattern('@app/models/item', star), 'models/item');
        assert.strictEqual(matchPathsPattern('@other/x', star), undefined);
        const exact = { pattern: 'config', prefix: 'config', suffix: undefined, targets: [] };
        assert.strictEqual(matchPathsPattern('config', exact), '');
        assert.strictEqual(matchPathsPattern('config/sub', exact), undefined);
    });
    it('rewrites only mapped specifiers and counts them', () => {
        const text = 'import { Item } from "@app/models/item";\nimport { z } from "zod";';
        const { text: out, rewrites } = rewriteSpecifiers(text, (spec) => spec.startsWith('@app/') ? './rewritten' : undefined);
        assert.strictEqual(rewrites, 1);
        assert.ok(out.includes('from "./rewritten"'));
        assert.ok(out.includes('from "zod"'));
    });
});
