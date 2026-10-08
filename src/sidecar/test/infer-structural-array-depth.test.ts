/**
 * carrick#1967, cause B: a list whose element has no symbol is still a list.
 *
 * A handler sends `rows.map(...)` building object literals, or a list typed by
 * an alias the compiler prints structurally. The model names the element
 * (`ItemDto`), bare by schema contract, so the use site's array-ness reaches
 * the capture only through the inference's `array_depth`. The inferrer used to
 * report that depth only beside a primary symbol, so a structural element
 * carried none, and the Rust depth join (`apply_inferred_array_depth`,
 * services/type_sidecar.rs) had nothing to copy onto the model's symbol.
 *
 * Pinned here: the depth is reported for a structural element at a send and at
 * a consumer's call result, and a named element keeps its symbol and depth.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ROUTES_TS = `interface Reply {
  send(body: unknown): void;
}

interface Row {
  id: string;
  title: string;
}

export interface ItemDto {
  id: string;
  title: string;
}

declare const rows: Row[];
declare function listItems(): Promise<{ id: string; title: string }[]>;
declare function listNamed(): Promise<ItemDto[]>;

export function sendMapped(reply: Reply) {
  const items = rows.map((row) => ({ id: row.id, title: row.title }));
  reply.send(items);
}

export function sendNamed(reply: Reply) {
  const named: ItemDto[] = rows;
  reply.send(named);
}

export async function readItems() {
  const items = await listItems();
  return items;
}

export async function readNamed() {
  const named = await listNamed();
  return named;
}
`;

interface Inferred {
  alias: string;
  type_string: string;
  primary_type_symbol?: string;
  array_depth?: number;
}

interface InferShape {
  inferred_types?: Inferred[];
}

describe('carrick#1967: an array with a structural element reports its depth', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1967-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
        },
        include: ['src/**/*.ts'],
      })
    );
    routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, ROUTES_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-1967', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  function lineOf(marker: string): number {
    const lines = ROUTES_TS.split('\n');
    const hits = lines.flatMap((line, index) => (line.includes(marker) ? [index + 1] : []));
    assert.strictEqual(hits.length, 1, `${marker} must name one line`);
    return hits[0];
  }

  async function infer(request: Record<string, unknown>): Promise<Inferred> {
    const alias = String(request.alias);
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [{ file_path: routesPath, ...request }],
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, `${alias} must be answered: ${JSON.stringify(res)}`);
    return inferred;
  }

  async function send(binding: string, alias: string): Promise<Inferred> {
    const line = lineOf(`reply.send(${binding})`);
    return infer({
      line_number: line,
      infer_kind: 'response_body',
      alias,
      expression_text: `reply.send(${binding})`,
      expression_line: line,
    });
  }

  async function read(loader: string, alias: string): Promise<Inferred> {
    const line = lineOf(`await ${loader}()`);
    return infer({
      line_number: line,
      infer_kind: 'call_result',
      alias,
      expression_text: `${loader}()`,
      expression_line: line,
    });
  }

  it('a send of mapped object literals reports depth 1 with no symbol', async () => {
    const inferred = await send('items', 'Mapped');
    assert.strictEqual(inferred.type_string, '{ id: string; title: string; }[]');
    assert.strictEqual(inferred.primary_type_symbol, undefined);
    assert.strictEqual(inferred.array_depth, 1);
  });

  it("a consumer's read of a structural list reports depth 1 with no symbol", async () => {
    const inferred = await read('listItems', 'Items');
    assert.strictEqual(inferred.type_string, '{ id: string; title: string; }[]');
    assert.strictEqual(inferred.primary_type_symbol, undefined);
    assert.strictEqual(inferred.array_depth, 1);
  });

  it('a named element keeps its symbol beside the depth', async () => {
    const sent = await send('named', 'Named');
    assert.strictEqual(sent.primary_type_symbol, 'ItemDto');
    assert.strictEqual(sent.array_depth, 1);
    const readBack = await read('listNamed', 'ReadNamed');
    assert.strictEqual(readBack.primary_type_symbol, 'ItemDto');
    assert.strictEqual(readBack.array_depth, 1);
  });
});
