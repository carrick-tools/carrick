/**
 * One floor on a consumer call result (carrick#4513 P1). The answer states
 * no type when it is a bare type parameter (whoever declares it) or when a
 * generic class an installed package declares stands where the body should
 * be: a result or future class around the body (`ResultAsync<Body, Err>`), or
 * a page class around its items. Neither is a body: JSON never makes a class
 * instance, and the class's own parameters print open in its declaration.
 *
 * What the floor keeps: a data interface or a non-generic class an SDK
 * declares, `Order[]` and `Order` read through `Array` and `Promise`,
 * `ArrayBuffer | null`, and a body shape that holds an open parameter only
 * nested inside it (the capture erases that position and keeps the shape).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const RESULT_PACKAGE = '@fixture/result';
const RESULT_DTS = `export declare class Ok<T, E> {
  readonly value: T;
  constructor(value: T);
  isOk(): this is Ok<T, E>;
  isErr(): this is Err<T, E>;
}
export declare class Err<T, E> {
  readonly error: E;
  constructor(error: E);
  isOk(): this is Ok<T, E>;
  isErr(): this is Err<T, E>;
}
export type Result<T, E> = Ok<T, E> | Err<T, E>;
export declare class ResultAsync<T, E> implements PromiseLike<Result<T, E>> {
  constructor(res: Promise<Result<T, E>>);
  static fromPromise<T, E>(promise: PromiseLike<T>, errorFn: (e: unknown) => E): ResultAsync<T, E>;
  andThen<U, F>(f: (t: T) => Result<U, F> | ResultAsync<U, F>): ResultAsync<U, E | F>;
  map<A>(f: (t: T) => A | Promise<A>): ResultAsync<A, E>;
  then<A = Result<T, E>, B = never>(
    successCallback?: ((res: Result<T, E>) => A | PromiseLike<A>) | null,
    failureCallback?: ((reason: unknown) => B | PromiseLike<B>) | null
  ): PromiseLike<A | B>;
}
export declare function okAsync<T, E = never>(value: T): ResultAsync<T, E>;
`;

const SDK_PACKAGE = '@fixture/sdk';
const SDK_DTS = `export interface Customer {
  id: string;
  email: string;
}
export declare class Invoice {
  id: string;
  total: number;
}
export declare class Page<Item> {
  data: Item[];
  hasMore(): boolean;
  next(): Promise<Page<Item>>;
}
export declare class ApiPromise<T> extends Promise<T> {}
export interface HttpResponse<T = any> {
  data: T;
  status: number;
  statusText: string;
  headers: Record<string, string>;
}
export declare const http: { get<T = any>(url: string): Promise<HttpResponse<T>> };
export declare class SdkClient {
  getCustomer(id: string): Promise<Customer>;
  getInvoice(id: string): Promise<Invoice>;
  listCustomers(): Promise<Page<Customer>>;
}
`;

const APP_TS = `import { ResultAsync, okAsync } from '${RESULT_PACKAGE}';
import { SdkClient, ApiPromise, http } from '${SDK_PACKAGE}';

export interface ResultBody { id: string; status: string }
export interface Order { id: string; total: number }
export type QueryBody = { format: 'json'; results: unknown[] } | { format: 'csv'; results: string };
export class AppError extends Error {}

export class DocClient {
  async getResult(id: string): Promise<ResultBody> {
    const res = await fetch(\`/api/v1/results/\${id}\`);
    return (await res.json()) as ResultBody;
  }
  async getOrder(id: string): Promise<Order> {
    const res = await fetch(\`/api/v1/orders/\${id}\`);
    return (await res.json()) as Order;
  }
  async listOrders(): Promise<Array<Order>> {
    const res = await fetch('/api/v1/orders');
    return (await res.json()) as Array<Order>;
  }
  query(text: string): ApiPromise<QueryBody> {
    return new ApiPromise<QueryBody>((resolve) => resolve({ format: 'csv', results: text }));
  }
}
export const docClient = new DocClient();
const sdk = new SdkClient();

// The service's call is handed to a library result class and read through it.
export function onStatus(id: string): ResultAsync<void, AppError> {
  return ResultAsync.fromPromise(docClient.getResult(id), (e) => new AppError(String(e))).andThen((body) =>
    okAsync<void, AppError>(record(body))
  );
}
function record(body: ResultBody): void {
  void body;
}

// The same, held in a name: the call's own result is still its body.
export function onStatusHeld(id: string) {
  const held = ResultAsync.fromPromise(docClient.getResult(id.trim()), (e) => new AppError(String(e)));
  return held;
}

// A wrapper whose return is the library result class with both parameters open.
export function wrapped<T, E>(run: () => Promise<T>, onError: (e: unknown) => E): ResultAsync<T, E> {
  return ResultAsync.fromPromise(run(), onError);
}

// A generic function stating a body shape that holds its parameter nested.
export async function execute<TRow>(text: string) {
  return docClient.query(text).then((r) => r) as Promise<
    { format: 'json'; results: TRow[] } | { format: 'csv'; results: string }
  >;
}

// A generic function stating its parameter as the whole body.
export async function readAs<T>(url: string) {
  const res = await fetch(url);
  const body = (await res.json()) as T;
  return body;
}

// A generic wrapper forwarding its caller's type argument to the client.
export async function callApi<T>(route: string): Promise<T> {
  const res = await http.get<T>(\`/api\${route}\`);
  return res.data;
}
export const skus = () => callApi<{ sku: string }>('/skus');

export const customer = async () => await sdk.getCustomer('c1');
export const invoice = async () => await sdk.getInvoice('i1');
export const customers = async () => await sdk.listCustomers();
export const orders = async () => await docClient.listOrders();
export const order = async () => await docClient.getOrder('o1');

export async function bytes(): Promise<ArrayBuffer | null> {
  const res = await fetch('/bytes');
  return res.ok ? res.arrayBuffer() : null;
}
`;

/** The rule a scan receives for the client's response object. */
const EXTRACTION_CONFIG = {
  rules: [
    {
      wrapperSymbols: ['HttpResponse'],
      machineryIndicators: ['status', 'statusText', 'headers'],
      originModuleGlobs: [SDK_PACKAGE, `${SDK_PACKAGE}/*`],
      payloadGenericIndex: 0,
      payloadPropertyPath: ['data'],
      unwrapRecursively: true,
      maxDepth: 4,
    },
  ],
};

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit?: boolean;
    any_provenance?: Array<{ path: string; reason: string }>;
    primary_type_symbol?: string;
    type_home?: { symbol?: string; file_path?: string };
  }>;
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('one floor on a consumer call result (carrick#4513 P1)', () => {
  let client: SidecarClient;
  let repoDir: string;
  let appPath: string;

  const writePackage = (name: string, dts: string) => {
    const dir = path.join(repoDir, 'node_modules', ...name.split('/'));
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' }));
    fs.writeFileSync(path.join(dir, 'index.d.ts'), dts);
  };

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-library-floor-'));
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
    writePackage(RESULT_PACKAGE, RESULT_DTS);
    writePackage(SDK_PACKAGE, SDK_DTS);
    appPath = path.join(repoDir, 'src', 'app.ts');
    fs.writeFileSync(appPath, APP_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-library-floor', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const spanOf = (text: string) => {
    const start = APP_TS.indexOf(text);
    assert.ok(start >= 0, `fixture holds no ${JSON.stringify(text)}`);
    return { start, end: start + text.length };
  };

  const infer = async (alias: string, call: string, decidingCall?: string) => {
    const { start } = spanOf(call);
    const through = decidingCall ? spanOf(decidingCall) : undefined;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: appPath,
          line_number: APP_TS.slice(0, start).split('\n').length,
          span_start: start,
          span_end: start + call.length,
          infer_kind: 'call_result',
          alias,
          ...(through
            ? { through_call: { file_path: appPath, span_start: through.start, span_end: through.end } }
            : {}),
        },
      ],
      extraction_config: EXTRACTION_CONFIG,
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(inferred, `no answer for ${alias}: ${JSON.stringify(response.errors)}`);
    const read = {
      text: collapse(inferred.type_string),
      provenance: (inferred.any_provenance ?? []).map((p) => p.reason),
      anchor: inferred.primary_type_symbol,
      home: inferred.type_home?.symbol,
    };
    if (process.env.FLOOR_DUMP) console.log(alias, JSON.stringify(inferred));
    return read;
  };

  const statesNoType = (read: Awaited<ReturnType<typeof infer>>, reason: string) => {
    assert.strictEqual(read.text, 'unknown', JSON.stringify(read));
    assert.deepStrictEqual(read.provenance, [reason], JSON.stringify(read));
    assert.strictEqual(read.anchor, undefined, JSON.stringify(read));
  };

  it('a call handed to a library result class never states the class', async () => {
    statesNoType(await infer('OnStatus', 'docClient.getResult(id)'), 'machinery_envelope');
  });

  it('a call handed to a library result class and held in a name keeps its own body', async () => {
    const read = await infer('OnStatusHeld', 'docClient.getResult(id.trim())');
    assert.strictEqual(read.text, 'ResultBody', JSON.stringify(read));
    assert.strictEqual(read.anchor, 'ResultBody', JSON.stringify(read));
  });

  it('a wrapper returning the library result class with both parameters open states no type', async () => {
    const read = await infer('Wrapped', 'ResultAsync.fromPromise(run(), onError)');
    assert.strictEqual(read.text, 'unknown', JSON.stringify(read));
    assert.strictEqual(read.anchor, undefined, JSON.stringify(read));
  });

  it('a generic page class an SDK declares around its items states no type', async () => {
    statesNoType(await infer('Customers', "sdk.listCustomers()"), 'machinery_envelope');
  });

  it('a stated bare type parameter states no type', async () => {
    statesNoType(await infer('ReadAs', 'fetch(url)'), 'bound_by_callers');
  });

  it('a stated body shape holding a parameter only nested keeps its shape', async () => {
    const read = await infer('Execute', 'docClient.query(text)');
    assert.match(read.text, /format: "json"; results: TRow\[\];/, JSON.stringify(read));
    assert.match(read.text, /format: "csv"; results: string;/, JSON.stringify(read));
    assert.deepStrictEqual(read.provenance, [], JSON.stringify(read));
  });

  it('a wrapper given an inline type argument anchors nothing, never its own parameter', async () => {
    const read = await infer('Skus', 'http.get<T>(`/api${route}`)', "callApi<{ sku: string }>('/skus')");
    assert.match(read.text, /^\{ sku: string;? \}$/, JSON.stringify(read));
    assert.strictEqual(read.anchor, undefined, JSON.stringify(read));
  });

  it('an SDK data interface keeps its type', async () => {
    const read = await infer('Customer', "sdk.getCustomer('c1')");
    assert.match(read.text, /(^|\.)Customer$/, JSON.stringify(read));
    assert.strictEqual(read.anchor, 'Customer', JSON.stringify(read));
  });

  it('a non-generic class an SDK declares keeps its type', async () => {
    const read = await infer('Invoice', "sdk.getInvoice('i1')");
    assert.match(read.text, /(^|\.)Invoice$/, JSON.stringify(read));
    assert.strictEqual(read.anchor, 'Invoice', JSON.stringify(read));
  });

  it('Array<Order> and Promise<Order> keep Order[] and Order', async () => {
    const list = await infer('Orders', 'docClient.listOrders()');
    assert.strictEqual(list.text, 'Order[]', JSON.stringify(list));
    assert.strictEqual(list.anchor, 'Order', JSON.stringify(list));
    const one = await infer('Order', "docClient.getOrder('o1')");
    assert.strictEqual(one.text, 'Order', JSON.stringify(one));
    assert.strictEqual(one.anchor, 'Order', JSON.stringify(one));
  });

  it('a TypeScript lib class keeps its type', async () => {
    const read = await infer('Bytes', 'res.arrayBuffer()');
    assert.strictEqual(read.text, 'ArrayBuffer | null', JSON.stringify(read));
    assert.deepStrictEqual(read.provenance, [], JSON.stringify(read));
  });
});
