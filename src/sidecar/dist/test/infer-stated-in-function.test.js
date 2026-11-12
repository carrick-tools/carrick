/**
 * A value the compiler types `any` or `unknown` takes the type the function it
 * is made in states for it (carrick#2594).
 *
 * The value is a consumer call's result or a handler's body read. The source
 * states its type somewhere else in the same function:
 *
 *  - returned unchanged from a function with a declared return type;
 *  - handed in place to a schema (`Schema.parse(await call())`), also behind
 *    an empty default (`Schema.safeParse(request.body ?? {})`);
 *  - handed whole to a typed parameter, a callable branch dropped;
 *  - assigned to a separately declared name (`let body: T; body = read`),
 *    whose annotation states it, or which a schema or a cast then reads;
 *  - handed to a function that returns it unchanged (a narrowing cast).
 *
 * And it stays untyped where the function states nothing about the value
 * itself: a function that CONVERTS it into something new (its return type
 * describes what it built), a `Promise<void>` that discards it, a placeholder
 * cast, and two casts that disagree.
 *
 * Every locator here is a text locator, the shape the scanner sends from the
 * model's `expression_text`. No library, method or framework name is matched.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const SERVICE_TS = `export interface Schema<Output, Input = Output> {
  parse(data: unknown): Output;
  safeParse(data: unknown): { success: true; data: Output } | { success: false };
  readonly _output: Output;
  readonly _input: Input;
}
declare function schemaOf<T>(): Schema<T>;

declare function callApi(method: string, path: string): Promise<any>;
declare function fetchJson(path: string): Promise<unknown>;

export type Created = { id: string; expiresAt: string };
export type Item = { id: string; title: string };

export const create = (): Promise<Created> => {
  return callApi('POST', '/v1/created');
};

export async function ping(): Promise<void> {
  await callApi('POST', '/v1/ping');
}

const ListSchema = schemaOf<{ items: Item[] }>();

export async function countItems(): Promise<number> {
  const { items } = ListSchema.parse(await fetchJson('/v1/items'));
  return items.length;
}

declare function setItems(next: Item[] | ((previous: Item[]) => Item[])): void;

export async function loadItems(): Promise<void> {
  const data = await callApi('GET', '/v1/stored');
  if (data && Array.isArray(data)) {
    setItems(data);
  }
}

export type Pull = { messages: { leaseId: string }[]; backlog: number };

function parsePull(value: unknown): Pull {
  const envelope = value as { result?: { messages?: { lease_id: string }[]; backlog_count?: number } };
  return {
    messages: (envelope.result?.messages ?? []).map((message) => ({ leaseId: message.lease_id })),
    backlog: envelope.result?.backlog_count ?? 0,
  };
}

export async function pull(): Promise<Pull> {
  const raw = await fetchJson('/v1/queue/pull');
  return parsePull(raw);
}

interface Incoming {
  json(): Promise<unknown>;
  text(): Promise<string>;
  body: unknown;
}

export type Upload = { name: string; size: number };

export async function uploadHandler(request: Incoming) {
  const raw = await request.text();
  let body: Upload;
  try {
    body = JSON.parse(raw);
  } catch {
    return { error: 'invalid json' };
  }
  return { saved: body.name };
}

const NoteSchema = schemaOf<{ title: string }>();

export async function noteHandler(request: Incoming) {
  let data: unknown;
  try {
    data = await request.json();
  } catch {
    return { error: 'invalid json' };
  }
  const result = NoteSchema.safeParse(data);
  return { ok: result.success };
}

export async function phoneHandler(request: Incoming) {
  let body: unknown;
  body = await request.json();
  if (typeof (body as Record<string, unknown>).phone !== 'string') {
    return { error: 'phone required' };
  }
  const phone = (body as { phone: string }).phone;
  return { phone };
}

export async function defaultedHandler(request: Incoming) {
  const parsed = NoteSchema.safeParse(request.body ?? {});
  return { ok: parsed.success };
}

export type Signal = { name?: string; at?: string };

function asSignal(value: unknown): Signal {
  if (!value || typeof value !== 'object') return {};
  return value as Signal;
}

export async function signalHandler(request: Incoming) {
  const signal = asSignal(request.body);
  return { name: signal.name };
}

function toTally(value: unknown): { tally: number } {
  return { tally: Array.isArray(value) ? value.length : 0 };
}

export async function tallyHandler(request: Incoming) {
  const tally = toTally(request.body);
  return tally;
}

export async function twoCastsHandler(request: Incoming) {
  const body = await request.json();
  const left = (body as { left: string }).left;
  const right = (body as { right: number }).right;
  return { left, right };
}
`;
describe('a value the compiler leaves untyped takes the type its own function states', () => {
    let client;
    let dir;
    let file;
    const lineOf = (text) => {
        const at = SERVICE_TS.indexOf(text);
        assert.ok(at >= 0, `fixture must contain: ${text}`);
        assert.strictEqual(SERVICE_TS.indexOf(text, at + 1), -1, `${text} must be unique`);
        return SERVICE_TS.slice(0, at).split('\n').length;
    };
    async function infer(alias, inferKind, expressionText, lineText) {
        const line = lineOf(lineText);
        const response = await client.send({
            action: 'infer',
            request_id: `stated-${alias}`,
            requests: [
                {
                    file_path: file,
                    line_number: line,
                    infer_kind: inferKind,
                    alias,
                    expression_text: expressionText,
                    expression_line: line,
                },
            ],
        });
        return response.inferred_types?.find((t) => t.alias === alias);
    }
    before(async () => {
        dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-stated-in-function-'));
        fs.writeFileSync(path.join(dir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: { target: 'ES2022', module: 'ESNext', moduleResolution: 'bundler', strict: true },
            include: ['src/**/*'],
        }));
        fs.mkdirSync(path.join(dir, 'src'));
        file = path.join(dir, 'src', 'service.ts');
        fs.writeFileSync(file, SERVICE_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'stated-init', repo_root: dir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(dir, { recursive: true, force: true });
    });
    it('a call returned unchanged takes its function’s declared return type', async () => {
        const inferred = await infer('Returned', 'call_result', "callApi('POST', '/v1/created')", "return callApi('POST', '/v1/created');");
        assert.match(inferred?.type_string ?? '', /id: string;\s*expiresAt: string/, JSON.stringify(inferred));
    });
    it('a call a Promise<void> function discards stays untyped', async () => {
        const inferred = await infer('Discarded', 'call_result', "callApi('POST', '/v1/ping')", "await callApi('POST', '/v1/ping');");
        assert.strictEqual(inferred?.type_string, 'any', JSON.stringify(inferred));
    });
    it('a call handed in place to a schema takes the schema’s input', async () => {
        const inferred = await infer('Parsed', 'call_result', "fetchJson('/v1/items')", "ListSchema.parse(await fetchJson('/v1/items'))");
        assert.match(inferred?.type_string ?? '', /items: \{ id: string; title: string; \}\[\]/, JSON.stringify(inferred));
    });
    it('a call handed to a typed parameter takes that parameter, its callable branch dropped', async () => {
        const inferred = await infer('Param', 'call_result', "callApi('GET', '/v1/stored')", "await callApi('GET', '/v1/stored');");
        assert.match(inferred?.type_string ?? '', /^\{ id: string; title: string; \}\[\]$/, JSON.stringify(inferred));
    });
    it('a call handed to a function that converts it stays untyped (the converter’s return is what it built)', async () => {
        const inferred = await infer('Converted', 'call_result', "fetchJson('/v1/queue/pull')", "await fetchJson('/v1/queue/pull');");
        assert.strictEqual(inferred?.type_string, 'unknown', JSON.stringify(inferred));
    });
    it('a body read assigned to an annotated name takes the annotation', async () => {
        const inferred = await infer('Assigned', 'request_body', 'JSON.parse(raw)', 'body = JSON.parse(raw);');
        assert.match(inferred?.type_string ?? '', /name: string;\s*size: number/, JSON.stringify(inferred));
    });
    it('a body read assigned to a name a schema then validates takes the schema’s input', async () => {
        const inferred = await infer('AssignedSchema', 'request_body', 'request.json()', 'data = await request.json();');
        assert.match(inferred?.type_string ?? '', /^\{ title: string; \}$/, JSON.stringify(inferred));
    });
    it('a body read assigned to a name the source casts whole takes the cast, not the placeholder', async () => {
        const inferred = await infer('AssignedCast', 'request_body', 'request.json()', '  body = await request.json();\n  if (typeof');
        assert.match(inferred?.type_string ?? '', /^\{ phone: string; \}$/, JSON.stringify(inferred));
    });
    it('a body read handed to a schema behind an empty default takes the schema’s input', async () => {
        const inferred = await infer('Defaulted', 'request_body', 'request.body', 'NoteSchema.safeParse(request.body ?? {})');
        assert.match(inferred?.type_string ?? '', /^\{ title: string; \}$/, JSON.stringify(inferred));
    });
    it('a body read handed to a function that returns it unchanged takes that function’s return type', async () => {
        const inferred = await infer('PassThrough', 'request_body', 'request.body', 'asSignal(request.body)');
        assert.match(inferred?.type_string ?? '', /name\?: string/, JSON.stringify(inferred));
    });
    it('a body read handed to a function that converts it stays untyped', async () => {
        const inferred = await infer('ConvertedBody', 'request_body', 'request.body', 'toTally(request.body)');
        assert.strictEqual(inferred?.type_string, 'unknown', JSON.stringify(inferred));
    });
    it('a body read cast whole to two different types states neither', async () => {
        const inferred = await infer('TwoCasts', 'request_body', 'request.json()', 'const body = await request.json();\n  const left');
        assert.strictEqual(inferred?.type_string, 'unknown', JSON.stringify(inferred));
    });
});
