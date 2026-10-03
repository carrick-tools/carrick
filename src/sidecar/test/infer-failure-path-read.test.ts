/**
 * Regression for carrick#1796: a consumer's body read on its failure path was
 * published as the call's response type.
 *
 * The def-use walk behind a `call_result` row takes the LAST whole read of the
 * call's result as the payload. A client that reads the success body inside
 * `if (res.ok) { ...; return ... }` and then reads the error text after it
 * (`const errorText = await res.text()`) published `string` as the response
 * contract. That text is what the server sends when the call FAILED, so it is
 * not the contract, and against a route that answers an object it is a false
 * incompatible.
 *
 * A read the source reaches only when the response failed describes the
 * failure. The walk does not look at it. "Failed" is read off the source's own
 * tests of the response and nothing else: `ok` is false, or the status is
 * outside 200-299. A test is decided only when EVERY status it lets through
 * failed. `res.status === 204` lets 200 through on its false side, so the
 * json read after `if (res.status === 204) return null` stays the payload.
 *
 * The runtime is the compiler's own DOM library, so `Response` is the platform
 * type a client really gets back from `fetch`. No library name is matched.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVICE_TS = `export interface Order {
  id: string;
  total: number;
}

export async function okThenText(): Promise<Order> {
  const res = await fetch("/orders/ok-then-text");
  if (res.ok) {
    return (await res.json()) as Order;
  }
  const detail = await res.text();
  throw new Error(detail);
}

export async function failFirst(): Promise<Order> {
  const res = await fetch("/orders/fail-first");
  if (!res.ok) {
    const detail = await res.text();
    throw new Error(detail);
  }
  return (await res.json()) as Order;
}

export async function textOnSuccess(): Promise<string> {
  const res = await fetch("/orders/text-on-success");
  if (!res.ok) {
    throw new Error("receipt failed");
  }
  const receipt = await res.text();
  return receipt;
}

export async function elseBranch(): Promise<Order> {
  const res = await fetch("/orders/else-branch");
  if (res.ok) {
    const order = (await res.json()) as Order;
    return order;
  } else {
    const detail = await res.text();
    throw new Error(detail);
  }
}

export async function statusBelow(): Promise<Order> {
  const res = await fetch("/orders/status-below");
  if (res.status < 400) {
    return (await res.json()) as Order;
  }
  const detail = await res.text();
  throw new Error(detail);
}

export async function errorJsonAfter(): Promise<Order> {
  const res = await fetch("/orders/error-json-after");
  if (res.ok) {
    return (await res.json()) as Order;
  }
  const problem = (await res.json()) as { message: string };
  throw new Error(problem.message);
}

export async function noContentFirst(): Promise<Order | null> {
  const res = await fetch("/orders/no-content-first");
  if (res.status === 204) return null;
  const order = (await res.json()) as Order;
  return order;
}

export async function removeOrder(): Promise<void> {
  const res = await fetch("/orders/remove", { method: "DELETE" });
  if (!res.ok) {
    const detail = await res.text();
    throw new Error(detail);
  }
}

export async function pingOrders(): Promise<void> {
  const res = await fetch("/orders/ping", { method: "DELETE" });
  if (!res.ok) {
    throw new Error("ping failed");
  }
}

export async function renderDocument(payload: string): Promise<Uint8Array> {
  for (let attempt = 1; attempt <= 2; attempt++) {
    const last = attempt === 2;
    let response: Response;
    try {
      response = await fetch("/render", { method: "POST", body: payload });
    } catch (error) {
      if (!last) continue;
      throw error;
    }
    if (response.ok) {
      const buffer = new Uint8Array(await response.arrayBuffer());
      if (buffer.byteLength === 0) {
        throw new Error("empty document");
      }
      return buffer;
    }
    if (response.status >= 500 && !last) {
      continue;
    }
    const errorText = await response.text().catch(() => "unknown error");
    throw new Error("render failed " + response.status + ": " + errorText);
  }
  throw new Error("exhausted");
}
`;

/** The 1-based line of the one source line containing `marker`. */
function lineOf(marker: string): number {
  const lines = SERVICE_TS.split('\n');
  const hits = lines.flatMap((line, index) => (line.includes(marker) ? [index + 1] : []));
  assert.strictEqual(hits.length, 1, `marker ${marker} must name one line`);
  return hits[0];
}

const ORDER_TEXT = '{ id: string; total: number; }';

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1796: a body read on the failure path is not the response type', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1796-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    servicePath = path.join(repoDir, 'src', 'service.ts');
    fs.writeFileSync(servicePath, SERVICE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The locator a scan really sends: the model's expression text and its line. */
  async function infer(alias: string, expressionText: string) {
    const line = lineOf(expressionText);
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: servicePath,
          line_number: line,
          infer_kind: 'call_result',
          alias,
          expression_text: expressionText,
          expression_line: line,
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('reads the success body, not the error text read after the success path returned', async () => {
    const inferred = await infer('Endpoint_OkThenText_Response', 'fetch("/orders/ok-then-text")');
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), ORDER_TEXT);
    assert.strictEqual(inferred.is_explicit, true);
  });

  it('reads the success body after a failure branch that throws (already held)', async () => {
    const inferred = await infer('Endpoint_FailFirst_Response', 'fetch("/orders/fail-first")');
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), ORDER_TEXT);
  });

  it('keeps a text read on the success path as the response', async () => {
    const inferred = await infer(
      'Endpoint_TextOnSuccess_Response',
      'fetch("/orders/text-on-success")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'string');
  });

  it('does not read the else branch of an ok test', async () => {
    const inferred = await infer('Endpoint_ElseBranch_Response', 'fetch("/orders/else-branch")');
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), ORDER_TEXT);
  });

  it('reads what follows a status test whose passing side returned as the failure path', async () => {
    const inferred = await infer('Endpoint_StatusBelow_Response', 'fetch("/orders/status-below")');
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), ORDER_TEXT);
  });

  it('does not take an error body parsed as json on the failure path', async () => {
    const inferred = await infer(
      'Endpoint_ErrorJsonAfter_Response',
      'fetch("/orders/error-json-after")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), ORDER_TEXT);
  });

  it('keeps the json read after an early return on 204, whose false side admits 200', async () => {
    const inferred = await infer(
      'Endpoint_NoContentFirst_Response',
      'fetch("/orders/no-content-first")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), ORDER_TEXT);
  });

  it('states what a call with no body read states when its only read is the error text', async () => {
    const inferred = await infer(
      'Endpoint_RemoveOrder_Response',
      'fetch("/orders/remove", { method: "DELETE" })'
    );
    const unread = await infer(
      'Endpoint_PingOrders_Response',
      'fetch("/orders/ping", { method: "DELETE" })'
    );
    assert.ok(inferred && unread, 'both rows must be answered');
    assert.notStrictEqual(
      collapse(inferred.type_string),
      'string',
      'the error text is what a failed call sends, so it is not the response contract'
    );
    assert.strictEqual(collapse(inferred.type_string), collapse(unread.type_string));
  });

  it('reads the bytes of a retried call, not the error text after its ok branch returned', async () => {
    const inferred = await infer(
      'Endpoint_RenderDocument_Response',
      'fetch("/render", { method: "POST", body: payload })'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.match(collapse(inferred.type_string), /^Uint8Array\b/);
  });
});
