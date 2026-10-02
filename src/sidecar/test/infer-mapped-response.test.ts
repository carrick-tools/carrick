/**
 * Regression for carrick#1732: a route that sends a MAPPED object was
 * published with the type of the row it was mapped from.
 *
 * The handler loads a database row and sends `toPublicView(row, ...)`, a
 * function in the same repo that builds a new object: dates as ISO strings,
 * big integers as strings, private columns left out. The model located that
 * call as the response expression, which is right. The response-body path
 * then treated every located call as a send it had to look inside
 * (`res.json(users)` -> `users`) and published the call's FIRST ARGUMENT, the
 * raw row. A consumer that reads the mapped shape was then judged against
 * columns the route never sends.
 *
 * A call whose result is a payload-shaped value that is not a library's own
 * object IS the payload, whoever declares the callee. The look-inside
 * fallback stays for what it was for: a send the transport types do not
 * describe (its result is `void`, `any` or `unknown`), and a call that hands
 * back a library's own object (a reply builder, a codec's writer).
 *
 * No framework or library name is matched: the rule reads what the call
 * evaluates to and where that type is declared.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

/** A library's send helper whose result is a builder, not transport machinery,
 * and a codec writer a repo encoder hands back. */
const LIBRARY_DTS = `export interface Reply {
  code(status: number): Reply;
  sent: boolean;
}
export declare function respond(body: unknown): Reply;
export interface Writer {
  finish(): Uint8Array;
  len: number;
}
export declare function toInstance<T>(cls: new () => T, plain: unknown): T;
`;

const SERVICE_TS = `import { respond, toInstance, type Writer } from "reply-kit";

export interface OrderRow {
  id: string;
  accessToken: string | null;
  buyerEmail: string | null;
  buyerName: string;
  placedAt: Date | null;
  totalMinor: bigint;
  lines: { id: string; quantity: bigint }[];
}

declare function findOrder(token: string): Promise<OrderRow | null>;
declare function send(body: unknown, init?: { status?: number }): void;

function toPublicView(order: OrderRow, note: string | null) {
  return {
    note,
    buyerName: order.buyerName,
    placedAt: order.placedAt?.toISOString() ?? null,
    totalMinor: order.totalMinor.toString(),
    lines: order.lines.map((item) => ({ id: item.id, quantity: item.quantity.toString() })),
  };
}

export async function publicRoute(token: string) {
  const order = await findOrder(token);
  if (!order) return send({ error: "not found" }, { status: 404 });
  return send(toPublicView(order, null));
}

export async function rawRoute(token: string) {
  const order = await findOrder(token);
  return send(order);
}

export async function libraryRoute(token: string) {
  const order = await findOrder(token);
  return respond(order);
}

declare function loadOrder(token: string): Promise<OrderRow>;
declare function encodeOrder(order: OrderRow): Writer;

export async function encodedRoute(token: string) {
  const loaded = await loadOrder(token);
  return send(encodeOrder(loaded));
}

export class OrderView {
  id = "";
  total = "";
}

export async function instanceRoute(token: string) {
  const loaded = await loadOrder(token);
  return send(toInstance(OrderView, loaded));
}
`;

const lineOf = (text: string): number => {
  const at = SERVICE_TS.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  assert.strictEqual(SERVICE_TS.indexOf(text, at + 1), -1, `fixture must contain exactly one: ${text}`);
  return SERVICE_TS.slice(0, at).split('\n').length;
};

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1732: a mapped response is the mapper result, not its first argument', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1732-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['es2022'] },
        include: ['src'],
      })
    );
    const library = path.join(repoDir, 'node_modules', 'reply-kit');
    fs.mkdirSync(library, { recursive: true });
    fs.writeFileSync(path.join(library, 'package.json'), JSON.stringify({ name: 'reply-kit', version: '1.0.0', types: 'index.d.ts' }));
    fs.writeFileSync(path.join(library, 'index.d.ts'), LIBRARY_DTS);
    servicePath = path.join(repoDir, 'src', 'service.ts');
    fs.writeFileSync(servicePath, SERVICE_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-1732', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (alias: string, expression: string, at: string): Promise<string> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: servicePath,
          line_number: lineOf(at),
          expression_text: expression,
          expression_line: lineOf(at),
          infer_kind: 'response_body',
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(inferred, `expected an inferred type for ${alias}, got ${JSON.stringify(response)}`);
    return collapse(inferred.type_string);
  };

  it('publishes the object the repo function builds, not the row passed into it', async () => {
    const text = await infer('Public', 'toPublicView(order, null)', 'return send(toPublicView(order, null));');
    for (const member of ['note: null', 'buyerName: string', 'placedAt: null | string', 'totalMinor: string', 'lines: { id: string; quantity: string; }[]']) {
      assert.ok(text.includes(member), `the mapped member \`${member}\` must be published, got: ${text}`);
    }
    for (const column of ['accessToken', 'buyerEmail', 'bigint', 'Date']) {
      assert.ok(!new RegExp(`\\b${column}\\b`).test(text), `\`${column}\` belongs to the row the route never sends, got: ${text}`);
    }
  });

  it('still reads the payload inside a send a library declares', async () => {
    const text = await infer('Library', 'respond(order)', 'return respond(order);');
    assert.ok(text.includes('accessToken: null | string'), `the library call is a send, its argument the payload, got: ${text}`);
    assert.ok(!/\bsent\b/.test(text), `the library's builder is not the route's contract, got: ${text}`);
  });

  it('still reads the payload inside a repo encoder that returns a library object', async () => {
    const text = await infer('Encoded', 'encodeOrder(loaded)', 'return send(encodeOrder(loaded));');
    assert.ok(text.includes('accessToken: null | string'), `the encoded message is the payload, got: ${text}`);
    assert.ok(!/\bfinish\b/.test(text), `the codec's writer is not the route's contract, got: ${text}`);
  });

  it('publishes the repo type a library call returns, not the class passed into it', async () => {
    const text = await infer('Instance', 'toInstance(OrderView, loaded)', 'return send(toInstance(OrderView, loaded));');
    assert.strictEqual(text, '{ id: string; total: string; }', `the instance the library builds is the payload, got: ${text}`);
  });

  it('still reads the payload inside a send the types say returns nothing', async () => {
    const text = await infer('Raw', 'send(order)', 'return send(order);');
    assert.ok(text.includes('accessToken: null | string'), `the sent row is the payload, got: ${text}`);
  });
});
