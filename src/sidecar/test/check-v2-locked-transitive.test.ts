/**
 * carrick#2091: the check installs a transitive dependency at the version the
 * scanned repo installed, not the newest version in the declared range.
 *
 * A pinned package declares its dependency with a caret range. The registry
 * lists a newer version in that range than the one the repo installed. The
 * producer stub records the installed edge in carrick-resolution.json; the
 * check must install that version.
 *
 * Offline: a local registry on 127.0.0.1, the real vendored pnpm, a scratch
 * store and cache per case, and the real tsc.
 */

import { describe, it, before } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
import type { CheckPairSpec, CheckResult, CheckStubInput } from '../src/capture/api.js';
import { localRegistry, useRegistry, type LocalRegistry, type RegistryPackage } from './helpers.js';

const PARENT: RegistryPackage = {
  name: '@fixture/parent',
  version: '1.0.0',
  manifest: { types: 'index.d.ts', dependencies: { '@fixture/child': '^1.0.0' } },
  files: { 'index.d.ts': "export type { Shape } from '@fixture/child';\n" },
};

const CHILD_LOCKED: RegistryPackage = {
  name: '@fixture/child',
  version: '1.0.0',
  manifest: { types: 'index.d.ts' },
  files: { 'index.d.ts': 'export type Shape = { a: string };\n' },
};

function writeStub(
  dir: string,
  service: string,
  surface: string,
  dependencies: Record<string, string>,
  resolution?: unknown
): CheckStubInput {
  const stubDir = path.join(dir, service);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify({
      name: `@carrick/${service}`,
      version: '0.0.0-carrick',
      private: true,
      types: './types/surface.d.ts',
      dependencies,
    })
  );
  fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
  if (resolution !== undefined) {
    fs.writeFileSync(path.join(stubDir, 'carrick-resolution.json'), JSON.stringify(resolution));
  }
  return { service_name: service, stub_dir: stubDir };
}

const PAIR: CheckPairSpec = {
  pair_key: 'shape',
  protocol: 'http',
  type_kind: 'response',
  producer: { service_name: 'orders', alias: 'P_Res' },
  consumer: { service_name: 'web', alias: 'C_Res' },
};

/** Run the check for one registry: producer pins the parent and records the locked edge. */
async function checkAgainst(packages: RegistryPackage[]): Promise<{ result: CheckResult; registry: LocalRegistry }> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-locked-transitive-'));
  const registry = await localRegistry(packages);
  const restore = useRegistry(registry, path.join(root, 'pnpm'));
  try {
    const stubs = [
      writeStub(
        path.join(root, 'stubs'),
        'orders',
        "export type P_Res = import('@fixture/parent').Shape;\n",
        { '@fixture/parent': '1.0.0' },
        { edges: { '@fixture/parent@1.0.0': { '@fixture/child': '1.0.0' } } }
      ),
      writeStub(path.join(root, 'stubs'), 'web', 'export type C_Res = { a: string };\n', {}),
    ];
    const result = await runCheck({ stubs, pairs: [PAIR], workspaceRoot: root });
    return { result, registry };
  } finally {
    restore();
    await registry.close();
    fs.rmSync(root, { recursive: true, force: true });
  }
}

describe('check_v2: a recorded transitive edge installs at the version the repo installed (#2091)', () => {
  describe('the newest in-range version cannot be fetched', () => {
    let result: CheckResult;
    let registry: LocalRegistry;
    before(async () => {
      ({ result, registry } = await checkAgainst([
        PARENT,
        CHILD_LOCKED,
        { ...CHILD_LOCKED, version: '1.1.0', unfetchable: true },
      ]));
    });

    it('the install succeeds', () => {
      assert.strictEqual(result.install_ok, true, JSON.stringify(result.errors));
      assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    });

    it('the pair is compared', () => {
      const v = result.verdicts.find((x) => x.pair_key === 'shape');
      assert.strictEqual(v?.bucket, 'compatible', JSON.stringify(v));
    });

    it('the unfetchable version was never requested', () => {
      assert.ok(
        !registry.tarballs.some((t) => t.endsWith('child-1.1.0.tgz')),
        registry.tarballs.join(', ')
      );
    });
  });

  describe('the newest in-range version carries a different type', () => {
    let result: CheckResult;
    before(async () => {
      ({ result } = await checkAgainst([
        PARENT,
        CHILD_LOCKED,
        {
          ...CHILD_LOCKED,
          version: '1.1.0',
          files: { 'index.d.ts': 'export type Shape = { a: number };\n' },
        },
      ]));
    });

    it('the pair is judged against the installed version', () => {
      assert.strictEqual(result.success, true, JSON.stringify(result.errors));
      const v = result.verdicts.find((x) => x.pair_key === 'shape');
      assert.strictEqual(v?.bucket, 'compatible', JSON.stringify(v));
    });
  });
});
