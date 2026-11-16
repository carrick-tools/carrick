/**
 * carrick#4689: a wrapper rule whose stated generic argument is open does not
 * take the next argument as the payload.
 *
 * A request library's response type carries the request-data type as a later
 * argument (`PostReply<T = any, D = any>`), and a query library's result
 * carries the variables the same way. With `T` unset the reader used to take
 * the first argument that was not open, so a call that sends a body published
 * the body as its response. The rule's own answers are the ones left: the
 * type at `payloadPropertyPath`, the declared return of the method, or the
 * decided `machinery_envelope` abstain. Never another argument.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { collapse, type WireInferred } from './wire-package.js';

const PACKAGE = 'tiny-post';

const PACKAGE_DTS = `export interface SentConfig<D = any> { url?: string; data?: D; headers?: Record<string, string> }
export interface PostReply<T = any, D = any> { data: T; status: number; statusText: string; headers: Record<string, string>; config: SentConfig<D> }
export interface PostClient {
  get<T = any, R = PostReply<T>, D = any>(url: string, config?: SentConfig<D>): Promise<R>;
  post<T = any, D = any>(url: string, data?: D, config?: SentConfig<D>): Promise<PostReply<T, D>>;
}
export interface QueryOutcome<TData = any, TVars = Record<string, unknown>> { data: TData; variables: TVars; loading: boolean }
export declare function runQuery<TData = any, TVars = Record<string, unknown>>(doc: string, variables: TVars): Promise<QueryOutcome<TData, TVars>>;
export declare function makeClient(): PostClient;
`;

const ORDERS_TS = `import { makeClient, runQuery } from "tiny-post";

export interface PlaceOrderInput {
  sku: string;
  quantity: number;
}
export interface PlaceOrderResult {
  order: { id: string; total: number };
}
export interface OrderQueryVars {
  orderId: string;
}

const client = makeClient();

export async function placeOrder(input: PlaceOrderInput): Promise<PlaceOrderResult> {
  const reply = await client.post('/orders', input);
  return reply.data;
}

export async function placeOrderLoose(input: PlaceOrderInput) {
  const reply = await client.post('/orders/loose', input);
  return reply.data;
}

export async function readOrder(id: string): Promise<PlaceOrderResult> {
  const reply = await client.get('/orders/' + id);
  return reply.data;
}

export async function placeOrderTyped(input: PlaceOrderInput) {
  const reply = await client.post<PlaceOrderResult>('/orders/typed', input);
  return reply.data;
}

export async function orderQuery(vars: OrderQueryVars) {
  const outcome = await runQuery('query Order { order { id } }', vars);
  return outcome.data;
}
`;

const RULES = [
  {
    wrapperSymbols: ['PostReply'],
    originModuleGlobs: [PACKAGE, `${PACKAGE}/*`],
    machineryIndicators: ['status', 'statusText', 'headers', 'config'],
    payloadGenericIndex: 0,
    payloadPropertyPath: ['data'],
    unwrapRecursively: true,
    maxDepth: 4,
  },
  {
    wrapperSymbols: ['QueryOutcome'],
    originModuleGlobs: [PACKAGE, `${PACKAGE}/*`],
    machineryIndicators: ['loading', 'variables'],
    payloadGenericIndex: 0,
    payloadPropertyPath: ['data'],
    unwrapRecursively: true,
    maxDepth: 4,
  },
];

describe('carrick#4689: an open generic argument is not replaced by the next one', () => {
  let client: SidecarClient;
  let repoDir: string;
  const file = (): string => path.join(repoDir, 'src', 'orders.ts');

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4689-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const pkgDir = path.join(repoDir, 'node_modules', PACKAGE);
    fs.mkdirSync(pkgDir, { recursive: true });
    fs.writeFileSync(
      path.join(pkgDir, 'package.json'),
      JSON.stringify({ name: PACKAGE, version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(pkgDir, 'index.d.ts'), PACKAGE_DTS);
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(file(), ORDERS_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** Infer the call on the line holding `needle`, with a locator like a scan's. */
  async function infer(alias: string, needle: string): Promise<WireInferred> {
    const lines = ORDERS_TS.split('\n');
    const index = lines.findIndex((line) => line.includes(needle));
    assert.ok(index >= 0, `the fixture must hold ${needle}`);
    const expression = lines[index]
      .trim()
      .replace(/^const \w+ = await /, '')
      .replace(/;$/, '');
    const res = await client.send<{ inferred_types?: WireInferred[] }>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: file(),
          line_number: index + 1,
          infer_kind: 'call_result',
          alias,
          expression_text: expression,
          expression_line: index + 1,
        },
      ],
      extraction_config: { rules: RULES },
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, 'the row must be answered');
    assert.doesNotMatch(inferred.type_string, /sku|quantity|orderId/, 'a request type is no response');
    return inferred;
  }

  function assertDecidedAbstain(inferred: WireInferred): void {
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
    const root = (inferred.any_provenance ?? []).filter((p) => p.path === '');
    assert.deepStrictEqual(
      root.map((p) => [p.kind, p.reason]),
      [['unknown', 'machinery_envelope']]
    );
  }

  it('a method that declares its return answers with it, not with the body it sends', async () => {
    const inferred = await infer('PlaceOrder_Response', "client.post('/orders', input)");
    assert.strictEqual(collapse(inferred.type_string), '{ order: { id: string; total: number; }; }');
  });

  it('a method with no declared return abstains, decided', async () => {
    assertDecidedAbstain(await infer('Loose_Response', "client.post('/orders/loose', input)"));
  });

  it("a query's variables are not its result", async () => {
    assertDecidedAbstain(await infer('Query_Response', 'runQuery('));
  });

  it('control: a call that sends nothing keeps the declared result', async () => {
    const inferred = await infer('ReadOrder_Response', "client.get('/orders/' + id)");
    assert.match(collapse(inferred.type_string), /order/);
  });

  it('control: an explicit argument is still the payload', async () => {
    const inferred = await infer('Typed_Response', "client.post<PlaceOrderResult>('/orders/typed', input)");
    assert.strictEqual(collapse(inferred.type_string), 'PlaceOrderResult');
  });
});
