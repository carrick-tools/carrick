/**
 * carrick#3497: a service whose only root config is `jsconfig.json` is typed
 * under it, not under a `tsconfig.json` above the service.
 *
 * tsserver types a JavaScript file under the jsconfig in its own directory,
 * so the service-root search names `jsconfig.json` (after the tsconfig names).
 * A service that keeps any tsconfig name at its root is unchanged.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { findServiceTsconfig } from '../src/capture/service-config.js';

const tempRoots: string[] = [];

after(() => {
  for (const dir of tempRoots) fs.rmSync(dir, { recursive: true, force: true });
});

/** A scan root holding `{}` configs at the given repo-relative paths. */
function scanRoot(configs: string[]): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'svc-jsconfig-')));
  tempRoots.push(root);
  for (const rel of configs) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), '{}');
  }
  return root;
}

describe('findServiceTsconfig: jsconfig.json at the service root', () => {
  it('prefers the service jsconfig.json to a tsconfig.json above the service', () => {
    const root = scanRoot(['tsconfig.json', 'web/jsconfig.json']);
    assert.strictEqual(findServiceTsconfig(path.join(root, 'web'), root), path.join(root, 'web', 'jsconfig.json'));
  });

  it('finds a lone service jsconfig.json with no scan root', () => {
    const root = scanRoot(['web/jsconfig.json']);
    assert.strictEqual(findServiceTsconfig(path.join(root, 'web')), path.join(root, 'web', 'jsconfig.json'));
  });

  it('keeps every tsconfig name ahead of jsconfig.json', () => {
    for (const name of ['tsconfig.json', 'tsconfig.build.json', 'tsconfig.app.json']) {
      const root = scanRoot([`web/${name}`, 'web/jsconfig.json']);
      assert.strictEqual(findServiceTsconfig(path.join(root, 'web'), root), path.join(root, 'web', name));
    }
  });

  it('still takes the ancestor tsconfig.json when the service has no root config', () => {
    const root = scanRoot(['tsconfig.json', 'web/src/x.ts']);
    assert.strictEqual(findServiceTsconfig(path.join(root, 'web'), root), path.join(root, 'tsconfig.json'));
  });
});
