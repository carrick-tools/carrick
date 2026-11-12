/**
 * carrick#2282: a call inside a function that sits inside a returned
 * expression was typed as that expression.
 *
 * The call-result walk asks first whether the call is returned, and it looked
 * for the nearest `return` above the call. That search does not stop at a
 * function boundary. A request made in a callback, where the callback is part
 * of what an OUTER function returns, was answered with the outer function's
 * returned expression: a command table, a client object, a UI element.
 *
 * A return statement answers only for a call of the function it returns from.
 *
 * Nothing here matches a name or a library: the rule is the language's own
 * scoping of `return`.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const SOURCE_TS = `interface Item { id: string }
interface Saved { saved: boolean }
interface UiElement { tag: string; props: unknown }
declare const api: {
  sync(id: string): Promise<Saved>;
  save(value: string): Promise<Saved>;
  one(id: string): Promise<Item>;
};
declare function element(tag: string, props: { onClick: () => Promise<void> }): UiElement;
declare const url: string;

export function commands() {
  return [{ name: 'sync', run: async (id: string) => { await api.sync(id); } }];
}

export function Panel(value: string) {
  return element('button', { onClick: async () => { await api.save(value); } });
}

export function makeClient() {
  return {
    list: async () => (await fetch(url + '/items')).json() as Promise<Item[]>,
    one: async (id: string) => {
      const res = await fetch(url + '/items/' + id);
      return (await res.json()) as Item;
    },
  };
}

export async function returnsTheCall(id: string) {
  return api.one(id);
}

export const expressionBody = (id: string) => api.one(id);

export async function returnsAWrapper(id: string) {
  return { item: await api.one(id), at: 1 };
}

interface ClientRequest {
  on(event: string, listener: (error: Error) => void): ClientRequest;
  end(): void;
}
interface IncomingMessage {
  on(event: 'data', listener: (chunk: string) => void): void;
  on(event: 'end', listener: () => void): void;
}
declare function sendRequest(target: string, onResponse: (res: IncomingMessage) => void): ClientRequest;

export function fetchText(target: string): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const req = sendRequest(target, (res) => {
      let text = '';
      res.on('data', (chunk) => {
        text += chunk;
      });
      res.on('end', () => resolve(text));
    });
    req.on('error', reject);
    req.end();
  });
}
`;
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
function lineOf(marker) {
    const index = SOURCE_TS.split('\n').findIndex((line) => line.includes(marker));
    assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
    return index + 1;
}
describe('carrick#2282: a return statement answers only for a call of its own function', () => {
    let client;
    let repoDir;
    let sourcePath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2282-'));
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
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        sourcePath = path.join(repoDir, 'src', 'source.ts');
        fs.writeFileSync(sourcePath, SOURCE_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    const callResult = async (alias, expression, at) => {
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: sourcePath,
                    line_number: lineOf(at),
                    expression_text: expression,
                    expression_line: lineOf(at),
                    infer_kind: 'call_result',
                    alias,
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        return inferred ? collapse(inferred.type_string) : undefined;
    };
    it('reads a call in a callback of a returned table as its own result, not the table', async () => {
        assert.strictEqual(await callResult('InTable', 'api.sync(id)', 'api.sync(id)'), 'Saved');
    });
    it('reads a call in a handler of a returned element as its own result, not the element', async () => {
        assert.strictEqual(await callResult('InElement', 'api.save(value)', 'api.save(value)'), 'Saved');
    });
    it('reads a call in a method of a returned client as the body it reads, not the client', async () => {
        assert.strictEqual(await callResult('InClient', "fetch(url + '/items')", "fetch(url + '/items')"), '{ id: string; }[]');
    });
    it('follows the body read of the function the call is in, inside a returned client', async () => {
        assert.strictEqual(await callResult('OwnReturn', "fetch(url + '/items/' + id)", "fetch(url + '/items/' + id)"), '{ id: string; }');
    });
    // Controls: a return still answers for a call of the function it returns from.
    it('still reads a function that returns the call itself', async () => {
        assert.strictEqual(await callResult('Returned', 'api.one(id)', 'return api.one(id)'), 'Item');
    });
    it('still reads an arrow with an expression body', async () => {
        assert.strictEqual(await callResult('Arrow', 'api.one(id)', 'expressionBody'), 'Item');
    });
    // The callback style: a request whose call returns the request object, made
    // inside the executor of the promise the outer function returns. What the
    // caller receives is what that promise settles with, not the request object.
    it('reads a request made in the executor of a returned promise as what the promise settles with', async () => {
        assert.strictEqual(await callResult('Executor', 'sendRequest(target, (res) => {', 'const req = sendRequest(target'), 'string');
    });
    // carrick#2628: an object the function builds around the call and returns
    // is the function's own value; the call's result is one member of it.
    it('reads a call inside a returned object literal as its own result, not the object', async () => {
        assert.strictEqual(await callResult('Wrapper', 'api.one(id)', 'item: await api.one(id)'), 'Item');
    });
});
/**
 * carrick#2112: a call's result was typed as a later expression the result
 * was passed into.
 *
 * Where the result is bound to a name, the walk follows the name and kept the
 * LAST expression that used it whole as the payload. That is right for an
 * expression that restates the value (an alias, a cast, a typed binding). It
 * is wrong for one that computes something else from it: `describe(page)`,
 * `Math.max(...items.map(…))`, `rows.map(…)`. Those published `string | null`,
 * `number` and `string[]` as the response of the call.
 *
 * A later expression moves the answer only when it restates the value: a
 * declaration whose initializer, with parentheses, `await`, `as`, `satisfies`
 * and `!` peeled, is the tracked name. Any other use that is not a member
 * read is a whole read, and the answer stays the call's own value.
 *
 * Two readings are kept on purpose. An expression that holds a platform body
 * read that is not json or text (`await res.blob()`) holds the body. And where the call's own value is untyped
 * (`unknown`, `any`), a call that takes it whole and returns it unchanged is
 * the first place the source states a type for it, so the walk still follows
 * it (carrick#1375, carrick#2595).
 */
const LATER_TS = `interface Page { items: { id: string; url: string }[]; nextCursor: string | null }
interface Item { id: string; name: string }
interface Wide { id: string; name: string; extra?: number }
declare const api: {
  getPage(id: string): Promise<Page>;
  list(): Promise<Item[]>;
  one(id: string): Promise<Item>;
};
declare function summarise(page: Page): string | null;
declare function readRaw(): Promise<unknown>;
function parseItem(value: unknown): Item { return value as Item; }
declare const url: string;

export async function passedToACall(id: string) {
  const page = await api.getPage(id);
  console.log(JSON.stringify(page));
  const note = summarise(page);
  console.log(note);
}

export async function spreadIntoACall(id: string) {
  const { items } = await api.getPage(id);
  console.log(JSON.stringify({ items }));
  const widest = Math.max(10, ...items.map((item) => item.url.length));
  console.log(widest);
}

export async function mappedInPlace() {
  const inPlaceNames = (await api.list()).map((x) => x.name);
  console.log(inPlaceNames);
}

export async function mappedLater() {
  const rows = await api.list();
  const laterNames = rows.map((x) => x.name);
  console.log(laterNames);
}

export async function behindACondition(id: string, show: boolean) {
  const shown = await api.one(id);
  const label = show && [shown, shown].length;
  console.log(label);
}

export async function castLater(id: string) {
  const castSource = await api.one(id);
  const wide = castSource as Wide;
  console.log(wide);
}

export async function typedBinding(id: string) {
  const typedSource = await api.one(id);
  const wide: Wide = typedSource;
  console.log(wide);
}

export async function satisfiesLater(id: string) {
  const checkedSource = await api.one(id);
  const checked = (checkedSource satisfies Item)!;
  console.log(checked);
}

export async function aliased(id: string) {
  const aliasSource = await api.one(id);
  const copy = aliasSource;
  console.log(copy);
}

export async function parsedBody() {
  const parsedRes = await fetch(url + '/parsed');
  const body = (await parsedRes.json()) as Item;
  console.log(body);
}

export async function blobBody() {
  const blobRes = await fetch(url + '/blob');
  const data = await blobRes.blob();
  console.log(data);
}

export async function bufferBody() {
  const bufferRes = await fetch(url + '/buffer');
  const bytes = new Uint8Array(await bufferRes.arrayBuffer());
  console.log(bytes);
}

declare function readJson<T>(response: Response, operation: string): Promise<T>;

export async function parsedThroughAHelper() {
  const helperRes = await fetch(url + '/token');
  const token: { access: string; expires: number } = await readJson(helperRes, 'exchange');
  console.log(token);
}

declare function fetchEnvelope(path: string): Promise<{ body: Item; response: Response }>;

export async function destructuredEnvelope() {
  const { body: envelopeBody } = await fetchEnvelope('/envelope');
  console.log(envelopeBody);
}

export async function untypedThenParsed() {
  const raw = await readRaw();
  const item = parseItem(raw);
  console.log(item);
}
`;
describe('carrick#2112: a later expression answers for a call only when it restates the value', () => {
    let client;
    let repoDir;
    let sourcePath;
    const laterLine = (marker) => {
        const index = LATER_TS.split('\n').findIndex((line) => line.includes(marker));
        assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
        return index + 1;
    };
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2112-'));
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
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        sourcePath = path.join(repoDir, 'src', 'later.ts');
        fs.writeFileSync(sourcePath, LATER_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init-2112', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    const callResult = async (alias, expression, at) => {
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: sourcePath,
                    line_number: laterLine(at),
                    expression_text: expression,
                    expression_line: laterLine(at),
                    infer_kind: 'call_result',
                    alias,
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        return inferred ? collapse(inferred.type_string) : undefined;
    };
    it('reads the call, not the call its result is passed to', async () => {
        assert.strictEqual(await callResult('Passed', 'api.getPage(id)', 'const page = await api.getPage(id)'), 'Page');
    });
    it('reads the call, not a value computed from a spread of its result', async () => {
        // A destructuring declaration has no type of its own (the checker answers
        // `any` for it), so the call answers for the value it takes apart.
        assert.strictEqual(await callResult('Spread', 'api.getPage(id)', 'const { items } = await api.getPage(id)'), 'Page');
    });
    it('reads the list the call declares where it is mapped in place', async () => {
        assert.strictEqual(await callResult('MappedInPlace', 'api.list()', 'const inPlaceNames'), 'Item[]');
    });
    it('reads the list the call declares where its binding is mapped later', async () => {
        assert.strictEqual(await callResult('MappedLater', 'api.list()', 'const rows = await api.list()'), 'Item[]');
    });
    it('reads the call, not an operator expression its result sits in', async () => {
        assert.strictEqual(await callResult('Condition', 'api.one(id)', 'const shown = await api.one(id)'), 'Item');
    });
    // Controls: an expression that restates the value still answers.
    it('still follows a later cast', async () => {
        assert.strictEqual(await callResult('Cast', 'api.one(id)', 'const castSource'), 'Wide');
    });
    it('still follows a typed binding', async () => {
        assert.strictEqual(await callResult('Typed', 'api.one(id)', 'const typedSource'), 'Wide');
    });
    it('still follows an alias, and one written through satisfies and a non-null assertion', async () => {
        assert.strictEqual(await callResult('Alias', 'api.one(id)', 'const aliasSource'), 'Item');
        assert.strictEqual(await callResult('Satisfies', 'api.one(id)', 'const checkedSource'), 'Item');
    });
    it('still reads a parsed body with the type stated around it', async () => {
        assert.strictEqual(await callResult('Parsed', "fetch(url + '/parsed')", 'const parsedRes'), '{ id: string; name: string; }');
    });
    it('still reads the expression that holds a platform body read that is not json or text', async () => {
        assert.strictEqual(await callResult('Blob', "fetch(url + '/blob')", 'const blobRes'), 'Blob');
        assert.strictEqual(await callResult('Buffer', "fetch(url + '/buffer')", 'const bufferRes'), 'Uint8Array<ArrayBuffer>');
    });
    // A helper that takes the transport response and reads its body, with the
    // body's type stated on the binding: the read of the body, as `res.json()`
    // is, and the annotation states it.
    it('still reads the body a helper takes out of the response, typed at its binding', async () => {
        assert.strictEqual(await callResult('ParseHelper', "fetch(url + '/token')", 'const helperRes'), '{ access: string; expires: number; }');
    });
    // A helper's own envelope around a transport response is never published
    // from the inferrer: its text would bypass the capture's machinery guard.
    it('leaves a destructured transport envelope to the capture', async () => {
        assert.strictEqual(await callResult('DestructuredEnvelope', "fetchEnvelope('/envelope')", 'const { body: envelopeBody }'), 'any');
    });
    it('still follows an untyped value into a function that returns it unchanged', async () => {
        assert.strictEqual(await callResult('Untyped', 'readRaw()', 'const raw = await readRaw()'), 'Item');
    });
});
