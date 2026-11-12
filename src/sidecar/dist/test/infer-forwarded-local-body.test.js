/**
 * carrick#2671: a call into an in-repo function that copies its argument into
 * a local and forwards that local, unchanged, to a helper that sends it as the
 * body.
 *
 *   export async function createPrice(token, org, input: PriceInput) {
 *     const payload: PriceInput = { label: norm(input.label), amount: input.amount };
 *     return await post<Res>(token, `/v1/${org}/prices`, payload);
 *   }
 *
 * The body sent is the local, so the request is the local's type. It is not
 * a decided `unknown` (the argument goes into a literal), and it is not the
 * argument's own type (the function rewrote it). A local the function builds
 * a further body from, or never sends, leaves the abstain in place.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const API_TS = `const BASE = 'https://api.example.test';
const log = (_entry: Record<string, unknown>, _message: string): void => {};
const norm = (code: string): string => code.toUpperCase();

async function post<T>(token: string, path: string, body: unknown): Promise<T> {
  const url = BASE + path;
  const response = await fetch(url, {
    method: 'POST',
    headers: { Authorization: token },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const body = await response.text().catch(() => '');
    log({ status: response.status, url, body }, 'POST failed');
    throw new Error(\`POST \${path}: HTTP \${response.status} - \${body}\`);
  }
  return (await response.json()) as T;
}

async function wrapPost(path: string, item: object) {
  return fetch(BASE + path, { method: 'POST', body: JSON.stringify({ item }) });
}

export interface PriceInput {
  label: string | null;
  amount: number | null;
}

export async function createPrice(token: string, org: number, input: PriceInput) {
  const payload: PriceInput = { label: norm(input.label ?? ''), amount: input.amount };
  return await post<{ id: string }>(token, \`/v1/\${org}/prices\`, payload);
}

export interface OrderInput {
  owner: string;
  code?: string;
}

export async function createOrder(token: string, org: number, order: OrderInput) {
  const payload = order.code == null ? order : { ...order, code: norm(order.code) };
  return await post<{ id: string }>(token, \`/v1/\${org}/orders\`, payload);
}

export async function notify(message: { text: string; channel?: string }) {
  const body = { channel: message.channel ?? 'general', text: \`[app] \${message.text}\` };
  return fetch(BASE + '/chat', { method: 'POST', body: JSON.stringify(body) });
}

export async function wrapItem(note: { label: string }) {
  const item = { label: norm(note.label) };
  return wrapPost('/v1/items', item);
}

export async function sendAndWrap(token: string, part: { code: string }) {
  const piece = { code: norm(part.code) };
  await post<{ ok: boolean }>(token, '/v1/pieces', piece);
  return fetch(BASE + '/v1/pieces/batch', { method: 'POST', body: JSON.stringify({ pieces: [piece] }) });
}

export async function sendAndForward(token: string, line: { sku: string }) {
  const entry = { sku: norm(line.sku) };
  await post<{ ok: boolean }>(token, '/v1/lines', entry);
  return wrapPost('/v1/lines/batch', entry);
}

export async function logOnly(token: string, change: { name: string }) {
  const record = { name: norm(change.name) };
  log({ record }, 'changing');
  return post<{ ok: boolean }>(token, '/v1/changes', { at: Date.now() });
}
`;
const CALLS_TS = `import { createPrice, createOrder, notify, wrapItem, sendAndWrap, sendAndForward, logOnly } from './api';

export async function run(token: string, org: number, id: string) {
  await createPrice(token, org, { label: id, amount: 1 });
  await createOrder(token, org, { owner: id, code: 'ab' });
  await notify({ text: id });
  await wrapItem({ label: id });
  await sendAndWrap(token, { code: id });
  await sendAndForward(token, { sku: id });
  await logOnly(token, { name: id });
}
`;
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick#2671: a function that forwards a local copy of its argument to a pass-through helper', () => {
    let client;
    let repoDir;
    let callsPath;
    before(async () => {
        repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2671-')));
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
    it('reads the declared local a function builds from its argument and forwards to a helper that sends it', async () => {
        const body = await request('{ label: id, amount: 1 }');
        assert.strictEqual(body, '{ label: null | string; amount: null | number; }');
    });
    it('reads a local that is the argument or a copy of it with one field rewritten', async () => {
        const body = await request("{ owner: id, code: 'ab' }");
        assert.strictEqual(body, '{ owner: string; code?: string; }');
    });
    it('reads the body a function builds into a local and sends itself, never the argument', async () => {
        const body = await request('{ text: id }');
        assert.strictEqual(body, '{ channel: string; text: string; }');
    });
    it('keeps the abstain when the forwarded local goes into a body the next helper builds', async () => {
        const body = await request('{ label: id }');
        assert.strictEqual(body, 'unknown (body_built_by_callee)');
    });
    it('keeps the abstain when the local is sent and also built into another body', async () => {
        const body = await request('{ code: id }');
        assert.strictEqual(body, 'unknown (body_built_by_callee)');
    });
    it('keeps the abstain when the local is sent and also handed to a helper that builds around it', async () => {
        const body = await request('{ sku: id }');
        assert.strictEqual(body, 'unknown (body_built_by_callee)');
    });
    it('keeps the abstain when the local built from the argument is never sent', async () => {
        const body = await request('{ name: id }');
        assert.strictEqual(body, 'unknown (body_built_by_callee)');
    });
});
