/**
 * carrick#4128: a call's request can be the body a wrapper builds
 * (`placeGeneric<T>(endpoint, item: T)` sending `{ body: item }`). The type
 * read at that body, inside the wrapper, is `{ body: T }`: the type
 * parameter is open there, and printed into a contract it names nothing.
 *
 * The scanner names the caller's call beside the body (`through_call`). The
 * body's type is then the one that call instantiates: `{ body: Book }` for
 * `placeGeneric('/orders', book)`. Where the call does not bind every type
 * parameter the body holds (a caller that passes its own type parameter, a
 * call that does not reach the function the body is in), no type naming an
 * open parameter is published: the answer is a decided `unknown`.
 *
 * The rule reads the checker's resolved signature, for any generic
 * function; no library or function name is matched. It holds for an HTTP
 * request body and for any other protocol's payload (`expression`).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const NET_TS = `export interface Book { title: string }
declare function send(path: string, options: unknown): Promise<unknown>;

export function placeGeneric<T>(endpoint: string, item: T) {
  return send(endpoint, { body: item });
}

export function placeMany<T>(endpoint: string, item: T) {
  return send(endpoint, { items: [item] });
}

export function placeLoose(endpoint: string, body: unknown) {
  return send(endpoint, { body });
}

export function placeKind<T>(endpoint: string, item: T) {
  return send(endpoint, { kind: 'order', item });
}
`;

const CALLER_TS = `import { placeGeneric, placeKind, placeMany, placeLoose, type Book } from './net';
declare const book: Book;
export const typed = () => placeGeneric('/orders', book);
export const literal = () => placeGeneric('/orders', 'book');
export const many = () => placeMany('/orders', book);
export const relay = <U>(item: U) => placeGeneric('/orders', item);
export const loose = () => placeLoose('/orders', 'book');
export const kinded = () => placeKind('/orders', book);
export const relayKind = <U>(item: U) => placeKind('/orders', item);
type Size = 'small' | 'large';
interface Parcel { size: Size }
declare const size: Parcel;
export const sized = () => placeGeneric('/orders', size);
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ reason: string; detail?: string }>;
    unwidened_type_string?: string;
    unwidened_out_of_budget?: boolean;
  }>;
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#4128: a body a generic wrapper builds takes its caller instantiation', () => {
  let client: SidecarClient;
  let repoDir: string;
  let netPath: string;
  let callerPath: string;

  const spanOf = (source: string, text: string): { start: number; end: number } => {
    const start = source.indexOf(text);
    assert.ok(start >= 0, `fixture holds no ${JSON.stringify(text)}`);
    assert.strictEqual(source.indexOf(text, start + 1), -1, `${JSON.stringify(text)} is not unique`);
    return { start, end: start + text.length };
  };
  const lineAt = (source: string, offset: number): number => source.slice(0, offset).split('\n').length;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4128-'));
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
    callerPath = path.join(repoDir, 'src', 'caller.ts');
    fs.writeFileSync(netPath, NET_TS);
    fs.writeFileSync(callerPath, CALLER_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-4128', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (
    alias: string,
    body: string,
    call: string | undefined,
    kind: 'request_body' | 'expression' = 'request_body'
  ) => {
    const at = spanOf(NET_TS, body);
    const through = call === undefined ? undefined : spanOf(CALLER_TS, call);
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: netPath,
          line_number: lineAt(NET_TS, at.start),
          span_start: at.start,
          span_end: at.end,
          infer_kind: kind,
          alias,
          ...(through
            ? { through_call: { file_path: callerPath, span_start: through.start, span_end: through.end } }
            : {}),
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(inferred, `no answer for ${alias}: ${JSON.stringify(response.errors)}`);
    return {
      text: collapse(inferred.type_string),
      provenance: inferred.any_provenance ?? [],
      unwidened: inferred.unwidened_type_string,
      unwidenedNotMade: inferred.unwidened_out_of_budget === true,
    };
  };

  it('reads the body as the caller instantiates the wrapper', async () => {
    const { text } = await infer('Typed', '{ body: item }', "placeGeneric('/orders', book)");
    assert.match(text, /^\{ body: \{ title: string; \}; \}$/, text);
  });

  it('reads a literal argument as its widened type', async () => {
    const { text } = await infer('Literal', '{ body: item }', "placeGeneric('/orders', 'book')");
    assert.strictEqual(text, '{ body: string; }');
  });

  it('instantiates a type parameter nested in the body', async () => {
    const { text } = await infer('Many', '{ items: [item] }', "placeMany('/orders', book)");
    assert.match(text, /^\{ items: \{ title: string; \}\[\]; \}$/, text);
  });

  it('answers a decided unknown where the caller passes its own type parameter', async () => {
    const { text, provenance } = await infer('Relay', '{ body: item }', "placeGeneric('/orders', item)");
    assert.strictEqual(text, 'unknown');
    assert.deepStrictEqual(
      provenance.map((p) => p.reason),
      ['body_built_by_callee']
    );
  });

  it('answers a decided unknown where the call does not reach the function the body is in', async () => {
    const { text, provenance } = await infer('Elsewhere', '{ body: item }', "placeLoose('/orders', 'book')");
    assert.strictEqual(text, 'unknown');
    assert.deepStrictEqual(
      provenance.map((p) => p.reason),
      ['body_built_by_callee']
    );
  });

  it('leaves a body with no type parameter as it reads', async () => {
    const { text } = await infer('Loose', '{ body }', "placeLoose('/orders', 'book')");
    assert.strictEqual(text, '{ body: unknown; }');
  });

  // The unwidened reading is made at the body, inside the wrapper, where the
  // parameter is open again: it is not made, and the answer says so, so a
  // widened literal never reads as a break.
  it('marks the unwidened reading not made where the wrapper writes a literal', async () => {
    const { text, unwidened, unwidenedNotMade } = await infer(
      'Kinded',
      "{ kind: 'order', item }",
      "placeKind('/orders', book)"
    );
    assert.strictEqual(text, '{ kind: string; item: { title: string; }; }');
    assert.strictEqual(unwidened, undefined);
    assert.strictEqual(unwidenedNotMade, true);
  });

  it('reads no unwidened type naming an open parameter', async () => {
    const { text, unwidened } = await infer('RelayKind', "{ kind: 'order', item }", "placeKind('/orders', item)");
    assert.strictEqual(text, 'unknown');
    assert.strictEqual(unwidened, undefined);
  });

  it('reads a type the caller declares and the wrapper cannot see', async () => {
    const { text } = await infer('Sized', '{ body: item }', "placeGeneric('/orders', size)");
    assert.strictEqual(text, '{ body: { size: "large" | "small"; }; }');
  });

  it('instantiates another protocol payload the same way', async () => {
    const { text } = await infer('Payload', '{ body: item }', "placeGeneric('/orders', 'book')", 'expression');
    assert.strictEqual(text, '{ body: string; }');
  });
});
