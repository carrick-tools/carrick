/**
 * carrick#2668, carrick#2664: a call into an in-repo request helper that
 * sends its argument as the body only when one is given.
 *
 * The helper's body slot is the argument, `JSON.stringify(argument)`, or a
 * conditional whose other branch sends nothing (`undefined`, `null`, `{}`):
 *
 *   body: data ? JSON.stringify(data) : undefined
 *   body: body ? JSON.stringify(body) : null
 *   ...(body === undefined ? {} : { body: JSON.stringify(body) })
 *
 * The argument is sent unchanged, so the request is the argument's own type,
 * generic helper or not. It is never the body slot's `string | undefined`,
 * and never a decided `unknown`. A helper that builds a new body around its
 * argument keeps reading that body.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const API_TS = `const BASE = 'https://api.example.test';
const log = (_entry: Record<string, unknown>): void => {};

export async function callApi(method: string, endpoint: string, data?: object) {
  const init: RequestInit = { method, body: data ? JSON.stringify(data) : undefined };
  return fetch(BASE + endpoint, init);
}

export async function post<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(BASE + path, { method: 'POST', body: JSON.stringify(body) });
  return (await res.json()) as T;
}

export class Api {
  async send<T>(method: string, path: string, body?: object): Promise<T> {
    const res = await fetch(BASE + path, { method, body: body ? JSON.stringify(body) : null });
    log({ path, status: res.status, body: body || null });
    return (await res.json()) as T;
  }
}

export async function request(method: string, path: string, body?: unknown): Promise<unknown> {
  const res = await fetch(BASE + path, {
    method,
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  return res.json();
}

export async function put(path: string, payload?: object) {
  log({ path, body: payload ?? undefined });
  return fetch(BASE + path, { method: 'PUT', body: payload ? JSON.stringify(payload) : undefined });
}

export async function wrapped(path: string, item?: { id: string }) {
  return fetch(BASE + path, { method: 'POST', body: item ? JSON.stringify({ item }) : undefined });
}
`;
const CALLS_TS = `import { callApi, post, Api, request, put, wrapped } from './api';

export async function run(api: Api, email: string, code: string, id: string) {
  await callApi('POST', '/v1/verify', { email, code });
  await post<{ ok: boolean }>('/v1/settled', { settled: true });
  await api.send<{ ok: boolean }>('PATCH', '/v1/items/1', { purchased: true });
  await request('POST', '/v1/items/query', { filter: { email }, limit: 1 });
  await put('/v1/items/2', { name: id });
  await wrapped('/v1/items', { id });
}
`;
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick#2668: a helper that sends its argument as the body when one is given', () => {
    let client;
    let repoDir;
    let callsPath;
    before(async () => {
        repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2668-')));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                lib: ['es2022', 'dom'],
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        fs.writeFileSync(path.join(repoDir, 'src', 'api.ts'), API_TS);
        callsPath = path.join(repoDir, 'src', 'calls.ts');
        fs.writeFileSync(callsPath, CALLS_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    /** The request answer for the argument `expression`, as a model locator names it; `unknown (<reason>)` when decided unknown. */
    async function request(expression) {
        const start = CALLS_TS.indexOf(expression);
        assert.ok(start >= 0, `source must contain ${expression}`);
        const line = CALLS_TS.slice(0, start).split('\n').length;
        const alias = `request_body:${expression}`;
        const res = await client.send({
            action: 'infer',
            request_id: alias,
            requests: [
                {
                    file_path: callsPath,
                    line_number: line,
                    expression_text: expression,
                    expression_line: line,
                    infer_kind: 'request_body',
                    alias,
                },
            ],
        });
        const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
        if (!inferred)
            return undefined;
        const root = (inferred.any_provenance ?? []).find((p) => p.path === '');
        const text = collapse(inferred.type_string);
        return text === 'unknown' && root ? `${text} (${root.reason})` : text;
    }
    it('reads the argument through `data ? JSON.stringify(data) : undefined`, never the slot value', async () => {
        const body = await request('{ email, code }');
        assert.strictEqual(body, '{ email: string; code: string; }');
    });
    it('reads the argument a generic helper sends as `JSON.stringify(body)`', async () => {
        const body = await request('{ settled: true }');
        assert.strictEqual(body, '{ settled: boolean; }');
    });
    it('reads the argument a generic helper sends as `body ? JSON.stringify(body) : null` and logs as `body || null`', async () => {
        const body = await request('{ purchased: true }');
        assert.strictEqual(body, '{ purchased: boolean; }');
    });
    it('reads the argument through a conditional spread `...(body === undefined ? {} : { body })`', async () => {
        const body = await request('{ filter: { email }, limit: 1 }');
        assert.strictEqual(body, '{ filter: { email: string; }; limit: number; }');
    });
    it('reads the argument a helper also logs as `payload ?? undefined`', async () => {
        const body = await request('{ name: id }');
        assert.strictEqual(body, '{ name: string; }');
    });
    it('reads the body a helper builds around its argument behind a conditional', async () => {
        const body = await request('{ id }');
        assert.strictEqual(body, '{ item: { id: string; }; }');
    });
});
