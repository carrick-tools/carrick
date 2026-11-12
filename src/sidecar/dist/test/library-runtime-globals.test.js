/**
 * carrick#2445 (slice 2): library claims on a global the runtime's types
 * package declares. The key is `node:<file under the types package, no
 * .d.ts>` (`node:web-globals/fetch`), the key the scanner computes for a
 * declaration in the runtime's types; its exports are the globals that file
 * declares.
 *
 * - The verifier reads a global as `const local = globalThis.<name>`, and its
 *   home is that file: a global the service adds in its own `declare global`
 *   block is not the runtime's.
 * - The lister lists the value globals that file declares, capped as a
 *   package is, and narrowed by `exports`.
 * - A `node:` key that names a module (`node:events`) is read as before.
 *
 * The runtime types package is vendored and minimal; the service is
 * hand-written.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const KEY = 'node:web-globals/fetch';
const TYPES_VERSION = '22.7.1';
let sequence = 0;
const check = (pkg, exported, claim) => ({
    claim_id: `r${sequence++}`,
    package: pkg,
    export: exported,
    role: 'http_client',
    receiver: 'export',
    claim,
});
const fetchShape = () => ({
    kind: 'op',
    op: 'request',
    member: null,
    name: { arg: 0 },
    method_key: { arg: 1, key: 'method' },
    method_default: 'GET',
    payload: { arg: 1, key: 'body' },
});
const verdicts = (response) => {
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response.verdicts.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
};
describe('library claims on a runtime types global (carrick#2445)', () => {
    const clients = [];
    let root = '';
    let client;
    let requestId = 0;
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-runtime-globals-'));
        const files = {
            'tsconfig.json': JSON.stringify({
                compilerOptions: {
                    target: 'es2020',
                    lib: ['es2020'],
                    module: 'commonjs',
                    moduleResolution: 'node',
                    strict: true,
                    skipLibCheck: true,
                    types: ['node'],
                },
                include: ['src/**/*.ts'],
            }),
            'package.json': JSON.stringify({ name: 'worker' }),
            'node_modules/@types/node/package.json': JSON.stringify({ name: '@types/node', version: TYPES_VERSION }),
            'node_modules/@types/node/index.d.ts': '/// <reference path="web-globals/fetch.d.ts" />\n',
            'node_modules/@types/node/web-globals/fetch.d.ts': 'declare function fetch(input: string, init?: { method?: string; body?: string }): Promise<{ ok: boolean }>;\n' +
                'declare const AbortSignalLike: { timeout(ms: number): unknown };\n' +
                'interface RequestInitLike { method?: string }\n',
            'src/index.ts': "export const ping = (): Promise<unknown> => fetch('/ping');\n",
            // The service gives the global scope a function of its own.
            'src/globals.ts': 'export {};\ndeclare global {\n  function track(url: string): void;\n}\n',
        };
        for (const [rel, text] of Object.entries(files)) {
            const file = path.join(root, rel);
            fs.mkdirSync(path.dirname(file), { recursive: true });
            fs.writeFileSync(file, text);
        }
        client = new SidecarClient();
        clients.push(client);
        await client.start();
        const init = await client.send({ request_id: `init-${requestId++}`, action: 'init', repo_root: root });
        assert.strictEqual(init.status, 'ready');
    });
    after(async () => {
        for (const each of clients)
            await each.stop();
        fs.rmSync(root, { recursive: true, force: true });
    });
    const verify = (checks) => client.send({ request_id: `verify-${requestId++}`, action: 'verify_library_claims', from_dir: root, checks }, 60_000);
    const list = async (packages, extra = {}) => {
        const response = await client.send({ request_id: `surface-${requestId++}`, action: 'list_library_surface', from_dir: root, packages, ...extra }, 120_000);
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        return response;
    };
    it('verifies a fetch claim against the types file, and answers export_missing for a name it does not declare', async () => {
        const response = await verify([
            check(KEY, 'fetch', fetchShape()),
            // Invalid: the default is no method word.
            check(KEY, 'fetch', { ...fetchShape(), method_default: 'SEND' }),
            // The service's own global is not the runtime's.
            check(KEY, 'track', { ...fetchShape(), method_default: 'POST' }),
            // A name no file declares.
            check(KEY, 'nothing', fetchShape()),
        ]);
        assert.deepStrictEqual(verdicts(response), [
            'verified',
            'failed method_not_word',
            'unchecked export_missing',
            'unchecked export_missing',
        ]);
        assert.strictEqual(response.modules.filter(entry => entry.package === KEY).length, 1);
        const module = response.modules.find(entry => entry.package === KEY);
        assert.strictEqual(module.installed_version, TYPES_VERSION);
        assert.ok(module.resolved_file.endsWith(path.join('web-globals', 'fetch.d.ts')));
        assert.strictEqual(module.reason, undefined);
    });
    it('answers module_unresolved for a file the program does not load, as a module name', async () => {
        assert.deepStrictEqual(verdicts(await verify([check('node:web-globals/absent', 'fetch', fetchShape())])), [
            'unchecked module_unresolved',
        ]);
    });
    it('lists the value globals the file declares, narrowed by exports, and caps the whole file', async () => {
        const { surfaces } = await list([KEY], { exports: { [KEY]: ['fetch', 'track', 'AbortSignalLike'] } });
        const [surface] = surfaces;
        assert.strictEqual(surface.package, KEY);
        assert.strictEqual(surface.installed_version, TYPES_VERSION);
        assert.strictEqual(surface.reason, undefined);
        // `track` is the service's; an interface is no value.
        assert.deepStrictEqual(surface.exports.map(entry => entry.export), ['AbortSignalLike', 'fetch']);
        assert.ok(surface.exports.find(entry => entry.export === 'fetch').receivers[0].call?.length);
        const whole = (await list([KEY])).surfaces[0];
        assert.deepStrictEqual(whole.exports.map(entry => entry.export), ['AbortSignalLike', 'fetch']);
        const capped = (await list([KEY], { max_entries: 1 })).surfaces[0];
        assert.ok(capped.exports.length <= 1 && capped.truncated > 0);
    });
});
