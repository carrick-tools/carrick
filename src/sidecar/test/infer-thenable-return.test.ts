/**
 * A resolver or handler that returns a library thenable it did not await
 * (a query builder whose result is `DbPromise<Order[]>`, a class with its own
 * `then`, not the built-in `Promise`) sends what `await` yields for it. The
 * function-return read used to publish the un-awaited wrapper, with its type
 * parameter open. The thenable class is declared in the throwaway source, so
 * the test needs no package.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import * as os from 'node:os';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';

interface InferResponseShape {
  request_id: string;
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    infer_kind: string;
    is_explicit: boolean;
    primary_type_symbol?: string;
  }>;
  errors?: string[];
}

const SOURCE = `interface Order {
  id: string;
  total: number;
}

class DbPromise<T> implements PromiseLike<T> {
  then<R1 = T, R2 = never>(
    onfulfilled?: ((value: T) => R1 | PromiseLike<R1>) | null,
    onrejected?: ((reason: unknown) => R2 | PromiseLike<R2>) | null
  ): PromiseLike<R1 | R2> {
    return Promise.resolve(undefined as unknown as T).then(onfulfilled, onrejected);
  }
}

declare const db: { findMany(args: { due: boolean }): DbPromise<Order[]> };

export function scheduledOrders() {
  return db.findMany({ due: true });
}

export function awaitedList(): Promise<Order[]> {
  return Promise.resolve([]);
}

export function plainList(): Order[] {
  return [];
}

export function passThrough<U>(query: DbPromise<U>) {
  return query;
}
`;

function spanOf(text: string): { start: number; end: number; line: number } {
  const start = SOURCE.indexOf(text);
  assert.ok(start >= 0, `source must contain: ${text}`);
  const line = SOURCE.slice(0, start).split('\n').length;
  return { start, end: start + text.length, line };
}

describe('a returned library thenable reads as what await yields', () => {
  let client: SidecarClient;
  let tmpDir: string;
  let fixtureFile: string;

  before(async () => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-thenable-return-'));
    fixtureFile = path.join(tmpDir, 'thenable-returns.ts');
    fs.writeFileSync(fixtureFile, SOURCE, 'utf-8');
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'thenable-init', repo_root: FIXTURES_PATH });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(tmpDir, { recursive: true, force: true });
  });

  async function inferReturn(signature: string, alias: string): Promise<any> {
    const fn = spanOf(signature);
    const response = await client.send<any>({
      action: 'infer',
      request_id: `thenable-${alias}`,
      requests: [
        {
          file_path: fixtureFile,
          line_number: fn.line,
          span_start: fn.start,
          span_end: fn.end,
          infer_kind: 'function_return',
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t: any) => t.alias === alias);
    assert.ok(inferred, `expected an inferred type for ${alias}: ${JSON.stringify(response.errors)}`);
    return inferred;
  }

  const ORDER_LIST = /^\{ id: string; total: number; \}\[\]$|^Array<\{ id: string; total: number; \}>$/;

  it('reads DbPromise<Order[]> as the awaited list', async () => {
    const inferred = await inferReturn('scheduledOrders()', 'Thenable');
    assert.ok(!/DbPromise/.test(inferred.type_string), `wrapper leaked: ${inferred.type_string}`);
    assert.match(inferred.type_string, ORDER_LIST);
  });

  it('control: Promise<Order[]> reads the same list', async () => {
    const inferred = await inferReturn('awaitedList(): Promise<Order[]>', 'Awaited');
    assert.match(inferred.type_string, ORDER_LIST);
  });

  it('control: a plain Order[] reads the same list', async () => {
    const inferred = await inferReturn('plainList(): Order[]', 'Plain');
    assert.match(inferred.type_string, ORDER_LIST);
  });

  it('an awaited type that still holds the open parameter is unknown, bound by callers', async () => {
    const inferred = await inferReturn('passThrough<U>(query: DbPromise<U>)', 'Open');
    assert.strictEqual(inferred.type_string, 'unknown');
    assert.ok(
      JSON.stringify(inferred.any_provenance).includes('bound_by_callers'),
      JSON.stringify(inferred)
    );
  });
});
