/**
 * carrick#2011: a consumer's request body built from literals is published
 * widened (`provider: string`), because a literal widens in an object
 * property. The call only ever sends the literal, so a producer that accepts
 * a literal union for that field accepts every value the call sends.
 *
 * The request inference carries, beside the published type, the body read
 * again with its literals kept (`unwidened_type_string`), as a response
 * inference does (carrick#1516). The judge reads it only to clear a mismatch
 * the published type raised.
 *
 * What a request's reading marks differs from a handler's in one way: a
 * literal handed to some other call (`useState('open')`, the `method: 'POST'`
 * option) is not marked. Keeping it narrow would narrow that call's generic
 * (the state's setter would accept only `'open'`), and the code that calls
 * the setter elsewhere, in a file the guard never checks, could send another
 * value. A literal inside the sent expression, or in a `const` it reads, is
 * marked.
 *
 * Shapes:
 * - a literal written in the body passed to `JSON.stringify`;
 * - a literal in an object sent as the call's argument directly;
 * - a `const` chosen by a conditional of two literals, then sent;
 * - a parameter declared `string`: nothing to mark, no reading;
 * - a state whose initial value is a literal handed to a generic call, written
 *   in the call or through a `const`: no reading;
 * - a body and a response read in one batch, in one function and in another
 *   function of the same file: the responses' rewrite comes first and must not
 *   cost the body its reading (review of PR #2710);
 * - an object built with a literal and then assigned another value: no
 *   reading (the guard sees the assignment).
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const CLIENT = `declare function apiFetch(url: string, init: { method: string; body: string }): Promise<unknown>;
declare function post<T>(url: string, body: T): Promise<unknown>;
declare function useState<T>(initial: T): [T, (next: T) => void];

export async function signup(token: string | undefined) {
  await apiFetch('/signup', {
    method: 'POST',
    body: JSON.stringify({ provider: 'GoogleOAuth', token }),
  });
}

export async function direct() {
  await post('/kind', { kind: 'archive', count: 2 });
}

export async function toggle(current: string) {
  const next = current === 'open' ? 'closed' : 'open';
  await apiFetch('/state', { method: 'PUT', body: JSON.stringify({ state: next }) });
}

export async function declared(state: string) {
  await apiFetch('/declared', { method: 'PUT', body: JSON.stringify({ state }) });
}

export function panel() {
  const [mode, setMode] = useState('open');
  void setMode;
  return apiFetch('/mode', { method: 'PUT', body: JSON.stringify({ mode }) });
}

export function panelConst() {
  const initial = 'open';
  const [mode, setMode] = useState(initial);
  void setMode;
  return apiFetch('/mode-const', { method: 'PUT', body: JSON.stringify({ mode: mode }) });
}

export async function charge() {
  await apiFetch('/billing', { method: 'POST', body: JSON.stringify({ kind: 'charge' }) });
  return { status: 'created' };
}

export function ready() {
  return { state: 'ready' };
}

export async function assigned(override: string | undefined) {
  const payload = { mode: 'full' };
  if (override) Object.assign(payload, { mode: override });
  await apiFetch('/assigned', { method: 'POST', body: JSON.stringify(payload) });
}

function applyOverride(b: { mode: string }, v: string) {
  b.mode = v;
}

export async function helped(v: string) {
  const options = { mode: 'full' };
  applyOverride(options, v);
  await apiFetch('/helped', { method: 'POST', body: JSON.stringify(options) });
}

export function relabelled(other: string) {
  const body = { state: 'open' };
  if (other) body.state = other;
  return apiFetch('/relabel', { method: 'PUT', body: JSON.stringify(body) });
}
`;
function lineOf(needle) {
    const at = CLIENT.indexOf(needle);
    assert.ok(at >= 0, `fixture must contain: ${needle}`);
    return CLIENT.slice(0, at).split('\n').length;
}
const collapse = (text) => text?.replace(/\s+/g, ' ').trim();
describe('carrick#2011: the unwidened reading of a consumer request body', () => {
    let client;
    let repoDir;
    let clientPath;
    let inferred;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2011-'));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022' },
            include: ['src'],
        }));
        clientPath = path.join(repoDir, 'src', 'client.ts');
        fs.writeFileSync(clientPath, CLIENT);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
        const body = (alias, text) => ({
            file_path: clientPath,
            line_number: lineOf(text),
            infer_kind: 'request_body',
            alias,
            expression_text: text,
            expression_line: lineOf(text),
        });
        const response = (alias, text) => ({
            ...body(alias, text),
            infer_kind: 'response_body',
        });
        const res = await client.send({
            action: 'infer',
            request_id: 'batch',
            requests: [
                body('signup', "JSON.stringify({ provider: 'GoogleOAuth', token })"),
                body('direct', "{ kind: 'archive', count: 2 }"),
                body('toggle', 'JSON.stringify({ state: next })'),
                body('declared', 'JSON.stringify({ state })'),
                body('panel', 'JSON.stringify({ mode })'),
                body('relabelled', 'JSON.stringify(body)'),
                body('panel-const', 'JSON.stringify({ mode: mode })'),
                body('assigned', 'JSON.stringify(payload)'),
                body('helped', 'JSON.stringify(options)'),
                body('charge-body', "JSON.stringify({ kind: 'charge' })"),
                response('charge-response', "{ status: 'created' }"),
                response('ready-response', "{ state: 'ready' }"),
            ],
        });
        assert.strictEqual(res.status, 'success', JSON.stringify(res));
        inferred = new Map((res.inferred_types ?? []).map((t) => [t.alias, t]));
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    it('keeps the published type widened: the compiler inference is the contract', () => {
        const t = inferred.get('signup');
        assert.ok(t, 'the body must infer');
        assert.match(collapse(t.type_string), /provider: string/);
    });
    it('reads a literal written in the serialised body', () => {
        const t = inferred.get('signup');
        const unwidened = collapse(t.unwidened_type_string);
        assert.ok(unwidened, `a reading is expected, got: ${JSON.stringify(t)}`);
        assert.match(unwidened, /provider: "GoogleOAuth"/);
        assert.match(unwidened, /token: string \| undefined/, 'a member with no literal keeps its type');
    });
    it('reads a literal in an object sent as the argument', () => {
        const unwidened = collapse(inferred.get('direct')?.unwidened_type_string);
        assert.ok(unwidened, JSON.stringify(inferred.get('direct')));
        assert.match(unwidened, /kind: "archive"/);
        assert.match(unwidened, /count: 2/);
    });
    it('reads a const chosen from two literals', () => {
        const unwidened = collapse(inferred.get('toggle')?.unwidened_type_string);
        assert.ok(unwidened, JSON.stringify(inferred.get('toggle')));
        assert.match(unwidened, /state: "closed" \| "open"|state: "open" \| "closed"/);
    });
    it('has no reading for a value the consumer declares string', () => {
        const t = inferred.get('declared');
        assert.ok(t, 'the body must infer');
        assert.strictEqual(t.unwidened_type_string, undefined, JSON.stringify(t));
    });
    it('never marks a literal handed to another call', () => {
        const t = inferred.get('panel');
        assert.ok(t, 'the body must infer');
        assert.strictEqual(t.unwidened_type_string, undefined, JSON.stringify(t));
    });
    it('never marks a const literal whose name is handed to another call', () => {
        const t = inferred.get('panel-const');
        assert.ok(t, 'the body must infer');
        assert.strictEqual(t.unwidened_type_string, undefined, JSON.stringify(t));
    });
    it('never marks a literal in an object handed to another call before it is sent', () => {
        for (const alias of ['assigned', 'helped']) {
            const t = inferred.get(alias);
            assert.ok(t, `${alias} must infer`);
            assert.strictEqual(t.unwidened_type_string, undefined, `${alias}: ${JSON.stringify(t)}`);
        }
    });
    it('reads a body and the responses beside it in one batch, in one function and in another', () => {
        for (const [alias, pattern] of [
            ['charge-body', /kind: "charge"/],
            ['charge-response', /status: "created"/],
            ['ready-response', /state: "ready"/],
        ]) {
            const t = inferred.get(alias);
            assert.ok(t, `${alias} must infer`);
            assert.match(collapse(t.unwidened_type_string) ?? '', pattern, `${alias}: ${JSON.stringify(t)}`);
            assert.strictEqual(t.unwidened_out_of_budget, undefined, alias);
        }
    });
    it('drops the reading when a kept literal is later assigned another value', () => {
        const t = inferred.get('relabelled');
        assert.ok(t, 'the body must infer');
        assert.strictEqual(t.unwidened_type_string, undefined, JSON.stringify(t));
    });
});
