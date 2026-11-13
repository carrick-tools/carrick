/**
 * carrick#4201: a response, call result, return or variable read inside a
 * generic function, with no caller named, still holds that function's type
 * parameter. Printed into a contract it names nothing: only the function's
 * callers set it. The answer is a decided `unknown` with `bound_by_callers`.
 *
 * The kinds that describe the generic function itself (`signature_return`,
 * `function_param`) keep the parameter: there it is the function's own type.
 * A function with no type parameter reads as it did.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const NET_TS = `export interface Book { title: string }
declare function send(path: string, options: unknown): Promise<unknown>;
declare function get<R>(path: string): Promise<R>;

export function readGeneric<T>(endpoint: string): Promise<T> {
  return send(endpoint, { generic: true }) as Promise<T>;
}

export async function readAwait<T>(endpoint: string) {
  const data = (await send(endpoint, { awaited: true })) as T;
  return data;
}

export async function fetchGeneric<T>(endpoint: string) {
  const page = await get<{ items: T[] }>(endpoint);
  return page;
}

export class Repo<E> {
  load(endpoint: string): Promise<E> {
    return get<E>(endpoint);
  }
}

export function readBook(endpoint: string): Promise<Book> {
  return send(endpoint, { plain: true }) as Promise<Book>;
}

export async function readBookAwait(endpoint: string) {
  const book = (await send(endpoint, { plainAwaited: true })) as Book;
  return book;
}

export async function fetchBook(endpoint: string) {
  const shelf = await get<{ items: Book[] }>(endpoint);
  return shelf;
}

export function keep<K>(item: K): K {
  return item;
}
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; reason: string; detail?: string }>;
    unwidened_type_string?: string;
  }>;
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#4201: a generic function’s own row publishes no open type parameter', () => {
  let client: SidecarClient;
  let repoDir: string;
  let netPath: string;
  let id = 0;

  /** The span of `text`, or of its first `length` characters. */
  const spanOf = (text: string, length = text.length): { start: number; end: number } => {
    const start = NET_TS.indexOf(text);
    assert.ok(start >= 0, `fixture holds no ${JSON.stringify(text)}`);
    assert.strictEqual(NET_TS.indexOf(text, start + 1), -1, `${JSON.stringify(text)} is not unique`);
    return { start, end: start + length };
  };
  const lineAt = (offset: number): number => NET_TS.slice(0, offset).split('\n').length;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4201-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    netPath = path.join(repoDir, 'src', 'net.ts');
    fs.writeFileSync(netPath, NET_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-4201', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** Infer `kind` at `text` (a span), or at the line `text` starts on when `byLine`. */
  const infer = async (
    kind: string,
    text: string,
    extra: Record<string, unknown> = {},
    byLine = false,
    length = text.length
  ) => {
    const at = spanOf(text, length);
    const alias = `A${id++}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: netPath,
          line_number: lineAt(at.start),
          ...(byLine ? {} : { span_start: at.start, span_end: at.end }),
          infer_kind: kind,
          alias,
          ...extra,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(inferred, `no answer for ${kind} at ${text}: ${JSON.stringify(response.errors)}`);
    return {
      text: collapse(inferred.type_string),
      reasons: (inferred.any_provenance ?? []).filter((p) => p.path === '').map((p) => p.reason),
      unwidened: inferred.unwidened_type_string,
    };
  };

  const assertBound = (answer: { text: string; reasons: string[]; unwidened?: string }, what: string) => {
    assert.strictEqual(answer.text, 'unknown', what);
    assert.deepStrictEqual(answer.reasons, ['bound_by_callers'], what);
    assert.strictEqual(answer.unwidened, undefined, what);
  };

  it('response_body: a cast to the promise of the parameter', async () => {
    assertBound(await infer('response_body', 'send(endpoint, { generic: true }) as Promise<T>'), 'cast');
  });

  it('response_body: an awaited read cast to the parameter', async () => {
    assertBound(await infer('response_body', '(await send(endpoint, { awaited: true })) as T'), 'awaited');
  });

  it('variable: a local holding the parameter', async () => {
    assertBound(await infer('variable', 'data = (await', {}, false, 'data'.length), 'variable');
  });

  it('function_return: the return of a generic function, read at the function', async () => {
    assertBound(await infer('function_return', 'export function readGeneric<T>', {}, true), 'return');
  });

  it('function_return: a method returning its class parameter', async () => {
    assertBound(await infer('function_return', 'load(endpoint: string): Promise<E>', {}, true), 'method');
  });

  it('call_result: a call whose result holds the parameter inside an object', async () => {
    assertBound(await infer('call_result', 'get<{ items: T[] }>(endpoint)'), 'call result');
  });

  it('reads a function with no type parameter as it did', async () => {
    const BOOK = /^(Book|\{ title: string; \})$/;
    assert.match((await infer('response_body', 'send(endpoint, { plain: true }) as Promise<Book>')).text, BOOK);
    assert.match((await infer('variable', 'book = (await', {}, false, 'book'.length)).text, /^(Book|\{ title: string; \})$/);
    assert.match((await infer('function_return', 'export function readBook', {}, true)).text, /^(Book|\{ title: string; \})$/);
    assert.match((await infer('call_result', 'get<{ items: Book[] }>(endpoint)')).text, /items: (Book|\{ title: string; \})\[\]/);
  });

  it('keeps the parameter where the row describes the generic function itself', async () => {
    const signature = await infer('signature_return', 'export function keep<K>', {}, true);
    assert.strictEqual(signature.text, 'K');
    const param = await infer('function_param', 'export function keep<K>', { param_name: 'item' }, true);
    assert.strictEqual(param.text, 'K');
  });
});
