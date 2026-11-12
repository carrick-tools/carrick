/**
 * carrick#3313: a service with no tsconfig is typed with the loader's default
 * compiler options. Those defaults paired `module: ESNext` with
 * `moduleResolution: NodeNext`, a pair TypeScript 6 rejects (TS5110). Under
 * NodeNext every `import` in such a program reads as ESM, so a relative
 * import written without an extension (`./config`) or naming a directory
 * (`./lib`) never resolved (TS2835): every type read through it was `any`,
 * and every position-row link through it was a hole.
 *
 * The defaults now resolve the way a bundler-style project does (the same
 * `Bundler` resolution capture's synthesized defaults already use), so both
 * imports resolve, with or without `"type": "module"` in package.json.
 */
import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const CLIENT_TS = `import { getSession } from "./config";
import { listUsers } from "./lib";

export async function session() {
  return getSession();
}

export async function users() {
  return listUsers();
}
`;
/** 1-based lines in CLIENT_TS. */
const SESSION_FN_LINE = 4;
const USERS_FN_LINE = 8;
const CONFIG_TS = `export interface Session { token: string; expires: number }
export async function getSession(): Promise<Session> {
  return { token: "t", expires: 0 };
}
`;
const LIB_INDEX_TS = `export interface User { id: string }
export async function listUsers(): Promise<User[]> {
  return [];
}
`;
const tempRoots = [];
after(() => {
    for (const root of tempRoots)
        fs.rmSync(root, { recursive: true, force: true });
});
/** A service with no tsconfig (nor jsconfig) anywhere up to its root. */
function writeService(packageType) {
    const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3313-')));
    tempRoots.push(root);
    const files = {
        'package.json': JSON.stringify({
            name: 'app',
            private: true,
            ...(packageType ? { type: packageType } : {}),
        }),
        'src/client.ts': CLIENT_TS,
        'src/config.ts': CONFIG_TS,
        'src/lib/index.ts': LIB_INDEX_TS,
    };
    for (const [rel, text] of Object.entries(files)) {
        const abs = path.join(root, rel);
        fs.mkdirSync(path.dirname(abs), { recursive: true });
        fs.writeFileSync(abs, text);
    }
    return root;
}
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
async function inferReturns(service) {
    const client = new SidecarClient();
    await client.start();
    try {
        const init = await client.send({
            action: 'init',
            request_id: 'init',
            repo_root: service,
            scan_root: service,
        });
        assert.strictEqual(init.status, 'ready', JSON.stringify(init));
        const file = path.join(service, 'src', 'client.ts');
        const res = await client.send({
            action: 'infer',
            request_id: 'returns',
            requests: [
                { file_path: file, line_number: SESSION_FN_LINE, infer_kind: 'function_return', alias: 'Session_Return' },
                { file_path: file, line_number: USERS_FN_LINE, infer_kind: 'function_return', alias: 'Users_Return' },
            ],
        }, 30000);
        return new Map((res.inferred_types ?? []).map((t) => [t.alias, collapse(t.type_string)]));
    }
    finally {
        await client.stop();
    }
}
describe('default compiler options resolve extensionless relative imports (carrick#3313)', () => {
    for (const packageType of [undefined, 'module']) {
        it(`types imports through an extensionless file and a directory index (package type: ${packageType ?? 'none'})`, async () => {
            const types = await inferReturns(writeService(packageType));
            const sessionType = types.get('Session_Return');
            const usersType = types.get('Users_Return');
            assert.ok(sessionType, `no inference for session(): ${JSON.stringify([...types])}`);
            assert.ok(usersType, `no inference for users(): ${JSON.stringify([...types])}`);
            assert.doesNotMatch(sessionType, /\bany\b/, `./config did not resolve: ${sessionType}`);
            assert.doesNotMatch(usersType, /\bany\b/, `./lib did not resolve: ${usersType}`);
            assert.match(sessionType, /token: string/);
            assert.match(sessionType, /expires: number/);
            assert.match(usersType, /id: string/);
        });
    }
});
