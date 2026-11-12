/**
 * carrick#3497: a service whose only root config is `jsconfig.json` is typed
 * under it, not under a `tsconfig.json` above the service.
 *
 * tsserver types a JavaScript file under the jsconfig in its own directory,
 * so a service-root `jsconfig.json` outranks a `tsconfig.json` above the service
 * (after the service's own tsconfig names). With no config above it, the answer
 * stays undefined and the per-file search reads the same jsconfig.
 * A service that keeps any tsconfig name at its root is unchanged.
 */
import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { findServiceTsconfig } from '../src/capture/service-config.js';
const tempRoots = [];
after(() => {
    for (const dir of tempRoots)
        fs.rmSync(dir, { recursive: true, force: true });
});
/** A scan root holding `{}` configs at the given repo-relative paths. */
function scanRoot(configs) {
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
    it('leaves a lone service jsconfig.json to the per-file search, as before', () => {
        const root = scanRoot(['web/jsconfig.json']);
        assert.strictEqual(findServiceTsconfig(path.join(root, 'web')), undefined);
        assert.strictEqual(findServiceTsconfig(path.join(root, 'web'), root), undefined);
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
