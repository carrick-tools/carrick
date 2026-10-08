/**
 * carrick#1841: a response table keyed by HTTP status code is not a body.
 *
 * A generated client types each operation by two tables, `{ 200: Item }` for
 * what a success carries and `{ 404: Problem }` for what a failure carries,
 * and answers `({ data: Item; error: undefined } | { data: undefined; error:
 * Problem }) & { request; response }`. The union is the client's bookkeeping.
 * What crossed the wire on success is the success table's body, `Item`.
 *
 * Two places publish that bookkeeping today, and both are fixed by one rule:
 * an object type whose every key is a status code is a response table, and
 * its body is its 2xx values.
 *
 *  - The inferred call result. The union carries no type arguments of its
 *    own; they are on the call (`client.get<Table, Errors>(…)`) or on the
 *    alias the wrapping function's declared return type was written with.
 *  - A symbol the model named for a consumer response (`GetItemResponses`):
 *    the v1 bundle and the capture both publish the table's body, not the
 *    table (see bundle-status-table.test.ts and
 *    capture-v2-status-table-symbol.test.ts).
 *
 * The fixture is a hand-written client of that shape with neutral names.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'status-table-client');

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    primary_type_symbol?: string;
    array_depth?: number;
    any_provenance?: Array<{ path: string; kind: string; reason: string }>;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();
const ITEM_TEXT = '{ id: string; name: string; }';

describe('carrick#1841: a status-keyed response table is not a body', () => {
  let client: SidecarClient;
  let repoDir: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1841-'));
    fs.cpSync(FIXTURE, repoDir, { recursive: true });
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function infer(file: string, line: number, expressionText: string) {
    const alias = `Endpoint_${line}_Response`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: path.join(repoDir, 'src', file),
          line_number: line,
          infer_kind: 'call_result',
          alias,
          expression_text: expressionText,
          expression_line: line,
        },
      ],
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, 'the row must be answered');
    return inferred;
  }

  it('reads the success body through the tables a call states as type arguments', async () => {
    const inferred = await infer(
      'sdk.gen.ts',
      12,
      "client.get<GetItemResponses, GetItemErrors>({ url: '/items/{id}', ...o })"
    );
    assert.ok(
      !/\berror:|\brequest:|\b200:/.test(inferred.type_string),
      `the result union and its tables are bookkeeping, got: ${inferred.type_string}`
    );
    assert.strictEqual(collapse(inferred.type_string), ITEM_TEXT);
    assert.strictEqual(inferred.primary_type_symbol, 'Item');
  });

  it('reads the success body at a caller of a function wrapping the call', async () => {
    // The caller writes no type arguments; the wrapping function's declared
    // return type was written with the tables.
    const inferred = await infer('callers.ts', 4, 'listItems()');
    assert.strictEqual(collapse(inferred.type_string), `${ITEM_TEXT}[]`);
    assert.strictEqual(inferred.primary_type_symbol, 'Item');
    assert.strictEqual(inferred.array_depth, 1);
  });

  it('reads the success body where the caller takes the data member out', async () => {
    const inferred = await infer('callers.ts', 9, 'listItems()');
    assert.strictEqual(collapse(inferred.type_string), `${ITEM_TEXT}[]`);
  });

  it('a success table whose only row is 204 states no body', async () => {
    const inferred = await infer(
      'sdk.gen.ts',
      19,
      "client.delete<DeleteItemResponses, DeleteItemErrors>({ url: '/items/{id}', ...o })"
    );
    assert.strictEqual(inferred.type_string.trim(), 'unknown', inferred.type_string);
    assert.strictEqual(inferred.primary_type_symbol, undefined);
    assert.deepStrictEqual(
      (inferred.any_provenance ?? []).map((p) => [p.path, p.reason]),
      [['', 'machinery_envelope']],
      'the abstain must be decided, or the capture re-reads the raw call'
    );
  });

  it('control: a table a non-generic call returns as its data is left as it is', async () => {
    const inferred = await infer('sdk.gen.ts', 24, "fetchCounts('/items/counts')");
    assert.strictEqual(collapse(inferred.type_string), '{ 200: number; 404: number; }');
  });
});
