/**
 * carrick#1819: an inference that names an anchor symbol also reports the
 * file that declares it, on every path that reports the symbol.
 *
 * The declaring file (now `type_home`, carrick#2148) was reported by a parameter read, an
 * expression read, and a response whose anchor was read off the annotation as
 * written. The paths an HTTP row takes (a function's return, a response
 * payload, a call's result, a payload recovered from a response send)
 * reported the symbol alone, so the scanner could name the type and not say
 * where to read it.
 *
 * Which declaration is the source is what the expression path already
 * answers, and every path now answers the same:
 *
 *  - a name imported through a barrel: the file that declares it, never the
 *    barrel that re-exports it;
 *  - a name two files declare: the file of the declaration this type
 *    resolves to;
 *  - a name an installed package declares: that package's declaration file,
 *    as an absolute path. What a reader is shown for such a home is the
 *    scanner's decision, not made here.
 *
 * No symbol, no source.
 *
 * carrick#2148: a shape declared as an object-literal alias
 * (`type Receipt = { … }`) or an instantiated generic alias (`Page<Invoice>`)
 * anchors at the alias's name and file, as an interface does. The compiler
 * gives such a type the anonymous `__type` as its own symbol; the alias is
 * where its name lives.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const CONTRACTS_TS = `export interface Invoice {
  id: string;
  total: number;
}
`;

/** Re-exports the name; declares nothing. */
const BARREL_TS = `export type { Invoice } from "./contracts";
`;

/** A second, unrelated declaration of the same name. */
const LEGACY_TS = `export interface Invoice {
  ref: string;
}
`;

/** Object-literal aliases: the shapes a type alias declares. */
const SHAPES_TS = `export type Receipt = {
  id: string;
  settled: boolean;
};

export type Page<T> = {
  items: T[];
  next: string | null;
};
`;

const PACKAGE_DTS = `export interface Stamp {
  at: string;
  by: string;
}
`;

const ROUTES_TS = `import type { Invoice } from "./index";
import type { Invoice as LegacyInvoice } from "./legacy";
import type { Stamp } from "doc-lib";
import type { Receipt, Page } from "./shapes";

interface Reply {
  json(body: unknown): void;
}

declare function emit(payload: unknown): void;
declare function findInvoice(id: string): Invoice;
declare function findLegacy(id: string): LegacyInvoice;
declare function findStamp(id: string): Stamp;
declare function loadInvoice(url: string): Promise<Invoice>;
declare function loadLegacy(url: string): Promise<LegacyInvoice>;
declare function loadStamp(url: string): Promise<Stamp>;
declare function findReceipt(id: string): Receipt;
declare function findPage(id: string): Page<Invoice>;
declare function loadReceipt(url: string): Promise<Receipt>;
declare function loadPage(url: string): Promise<Page<Invoice>>;

export function getReceipt(id: string) {
  return findReceipt(id);
}

export function getPage(id: string) {
  return findPage(id);
}

export function sendReceipt(id: string, reply: Reply) {
  const receipt = findReceipt(id);
  reply.json(receipt);
}

export function sendPage(id: string, reply: Reply) {
  const page = findPage(id);
  reply.json(page);
}

export async function fetchReceipt(id: string) {
  const receipt = await loadReceipt(\`/receipts/\${id}\`);
  return receipt;
}

export async function fetchPage(id: string) {
  const page = await loadPage(\`/pages/\${id}\`);
  return page;
}

export function getInvoice(id: string) {
  return findInvoice(id);
}

export function getLegacy(id: string) {
  return findLegacy(id);
}

export function getStamp(id: string) {
  return findStamp(id);
}

export function listInvoices(id: string) {
  return [findInvoice(id)];
}

export function getInline(id: string) {
  return { id, ok: true };
}

export function sendInvoice(id: string, reply: Reply) {
  const invoice = findInvoice(id);
  reply.json(invoice);
}

export function sendLegacy(id: string, reply: Reply) {
  const legacy = findLegacy(id);
  reply.json(legacy);
}

export function sendStamp(id: string, reply: Reply) {
  const stamp = findStamp(id);
  reply.json(stamp);
}

export async function fetchInvoice(id: string) {
  const invoice = await loadInvoice(\`/invoices/\${id}\`);
  return invoice;
}

export async function fetchLegacy(id: string) {
  const legacy = await loadLegacy(\`/legacy/\${id}\`);
  return legacy;
}

export async function fetchStamp(id: string) {
  const stamp = await loadStamp(\`/stamps/\${id}\`);
  return stamp;
}

export function publishInvoice(id: string) {
  const invoice = findInvoice(id);
  emit(invoice);
}

export function publishLegacy(id: string) {
  const legacy = findLegacy(id);
  emit(legacy);
}

export function publishStamp(id: string) {
  const stamp = findStamp(id);
  emit(stamp);
}

export function replyInvoice(id: string): Response {
  const invoice = findInvoice(id);
  return Response.json(invoice);
}
`;

interface Inferred {
  alias: string;
  type_string: string;
  is_explicit?: boolean;
  primary_type_symbol?: string;
  type_home?: Home2148;
  array_depth?: number;
  stated_body?: { root?: string; root_home?: Home2148 };
}

/** carrick#2148: where the type an answer printed is declared. */
interface Home2148 {
  symbol: string;
  file_path: string;
  line: number;
}

interface InferShape {
  inferred_types?: Inferred[];
}

/** The three declaration homes, by the binding that holds a value of each. */
type Home = 'invoice' | 'legacy' | 'stamp' | 'receipt' | 'page';

describe('carrick#1819: an inference that names an anchor symbol reports the file that declares it', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1819-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const pkgDir = path.join(repoDir, 'node_modules', 'doc-lib');
    fs.mkdirSync(pkgDir, { recursive: true });
    fs.writeFileSync(
      path.join(pkgDir, 'package.json'),
      JSON.stringify({ name: 'doc-lib', version: '1.0.0', types: 'index.d.ts' })
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
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'contracts.ts'), CONTRACTS_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'index.ts'), BARREL_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'legacy.ts'), LEGACY_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'shapes.ts'), SHAPES_TS);
    routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, ROUTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The 1-based line of the one source line containing `marker`. */
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
    assert.ok(inferred, `${alias} must be answered`);
    return inferred;
  }

  const FUNCTION_OF: Record<Home, string> = {
    invoice: 'getInvoice',
    legacy: 'getLegacy',
    stamp: 'getStamp',
    receipt: 'getReceipt',
    page: 'getPage',
  };
  const LOADER_OF: Record<Home, string> = {
    invoice: 'loadInvoice',
    legacy: 'loadLegacy',
    stamp: 'loadStamp',
    receipt: 'loadReceipt',
    page: 'loadPage',
  };

  /** One request per path, for the value declared at `home`. */
  const PATHS: Record<string, (home: Home) => Promise<Inferred>> = {
    "a function's return": (home) =>
      infer({
        line_number: lineOf(`function ${FUNCTION_OF[home]}(`),
        infer_kind: 'function_return',
        alias: `Return_${home}`,
      }),
    'a response payload': (home) => {
      const line = lineOf(`reply.json(${home})`);
      return infer({
        line_number: line,
        infer_kind: 'response_body',
        alias: `Payload_${home}`,
        expression_text: `reply.json(${home})`,
        expression_line: line,
      });
    },
    "a call's result": (home) => {
      const line = lineOf(`await ${LOADER_OF[home]}(`);
      const text = ROUTES_TS.split('\n')[line - 1].match(/load\w+\(`[^`]+`\)/)?.[0];
      return infer({
        line_number: line,
        infer_kind: 'call_result',
        alias: `Call_${home}`,
        expression_text: text,
        expression_line: line,
      });
    },
  };

  /** The expression path, which has reported the source since carrick#413. */
  function expression(home: Home): Promise<Inferred> {
    const line = lineOf(`emit(${home})`);
    return infer({
      line_number: line,
      infer_kind: 'expression',
      alias: `Expression_${home}`,
      expression_text: home,
      expression_line: line,
    });
  }

  const inRepo = (file: string): string => path.join(repoDir, 'src', file);

  for (const [name, request] of Object.entries(PATHS)) {
    describe(name, () => {
      it('reports the file that declares a name imported through a barrel', async () => {
        const inferred = await request('invoice');
        assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
        assert.deepStrictEqual(
          inferred.type_home,
          { symbol: 'Invoice', file_path: inRepo('contracts.ts'), line: 1 },
          'the barrel re-exports the name and declares nothing'
        );
      });

      it('reports the declaration this type resolves to where two files declare the name', async () => {
        const inferred = await request('legacy');
        assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
        assert.deepStrictEqual(inferred.type_home, { symbol: 'Invoice', file_path: inRepo('legacy.ts'), line: 1 });
      });

      it("reports an installed package's declaration file for a name it declares", async () => {
        const inferred = await request('stamp');
        assert.strictEqual(inferred.primary_type_symbol, 'Stamp');
        assert.deepStrictEqual(inferred.type_home, {
          symbol: 'Stamp',
          file_path: path.join(repoDir, 'node_modules', 'doc-lib', 'index.d.ts'),
          line: 1,
        });
      });

      it('anchors an object-literal alias at its name and file (carrick#2148)', async () => {
        const inferred = await request('receipt');
        assert.strictEqual(inferred.primary_type_symbol, 'Receipt');
        assert.deepStrictEqual(inferred.type_home, { symbol: 'Receipt', file_path: inRepo('shapes.ts'), line: 1 });
      });

      it('anchors an instantiated generic alias at its bare name (carrick#2148)', async () => {
        const inferred = await request('page');
        assert.strictEqual(inferred.primary_type_symbol, 'Page');
        assert.deepStrictEqual(inferred.type_home, { symbol: 'Page', file_path: inRepo('shapes.ts'), line: 6 });
      });

      it('answers as the expression path does for each of the three', async () => {
        for (const home of ['invoice', 'legacy', 'stamp'] as const) {
          const here = await request(home);
          const there = await expression(home);
          assert.ok(there.type_home, 'the expression path reports a home');
          assert.deepStrictEqual(
            [here.primary_type_symbol, here.type_home],
            [there.primary_type_symbol, there.type_home],
            `${home}: one symbol, one home, whichever path names it`
          );
        }
      });
    });
  }

  it('reports the element type\'s file for an array payload', async () => {
    const inferred = await infer({
      line_number: lineOf('function listInvoices('),
      infer_kind: 'function_return',
      alias: 'Return_list',
    });
    assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    assert.strictEqual(inferred.array_depth, 1);
    assert.deepStrictEqual(inferred.type_home, { symbol: 'Invoice', file_path: inRepo('contracts.ts'), line: 1 });
  });

  it('reports the file for a payload recovered from a response send', async () => {
    // The handler returns transport; the payload is the argument of the send,
    // and its resolved type carries the symbol.
    const inferred = await infer({
      line_number: lineOf('function replyInvoice('),
      infer_kind: 'function_return',
      alias: 'Return_recovered',
    });
    assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    assert.deepStrictEqual(inferred.type_home, { symbol: 'Invoice', file_path: inRepo('contracts.ts'), line: 1 });
  });

  it('reports no source where there is no symbol', async () => {
    const inferred = await infer({
      line_number: lineOf('function getInline('),
      infer_kind: 'function_return',
      alias: 'Return_inline',
    });
    assert.strictEqual(inferred.primary_type_symbol, undefined);
    assert.strictEqual(inferred.type_home, undefined);
  });
});

/**
 * carrick#2148 H1: every answer that prints a named type reports where that
 * type is declared, `type_home: { symbol, file_path, line }`, the line being
 * the declaration's name. The home is the type the text was printed from,
 * with array levels and a `| null`/`| undefined` around one named type
 * peeled. A library type, a package's type declared in an ambient
 * `declare module` block, and a shape nobody named have none.
 */
const H1_KIT_DTS = `export interface Res {
  status(code: number): Res;
  json(body: unknown): Res;
}
export interface Request<P = {}, Q = {}, B = any> {
  params: P;
  query: Q;
  body: B;
  json(): Promise<unknown>;
}
export type Handler = (req: Request<any, any, any>, res: Res) => unknown;
export declare class App {
  get(path: string, handler: Handler): App;
  post(path: string, handler: Handler): App;
}
`;

const H1_CONTRACTS_TS = `export interface Invoice {
  id: string;
  total: number;
}

export interface Order {
  id: string;
}

/**
 * What a client sends to create an order.
 * Kept small on purpose.
 */
export interface CreateOrder {
  sku: string;
  quantity: number;
}

export interface Item {
  name: string;
}

export interface NewWidget {
  name: string;
  size: number;
}

export type Receipt = {
  id: string;
  settled: boolean;
};

export type Page<T> = {
  items: T[];
  next: string | null;
};
`;

const H1_BARREL_TS = `export * from "./contracts";
`;

const H1_TICKET_TS = `export default interface Ticket {
  seat: string;
}
`;

/** A package's types, declared by the repo in an ambient module block. */
const H1_VENDOR_DTS = `declare module "http-kit" {
  export interface KitResponse<T> {
    data: T;
    status: number;
  }
  export function get<T>(url: string): Promise<KitResponse<T>>;
}
`;

const H1_ROUTES_TS = `import { App, type Request } from "res-kit";
import { get } from "http-kit";
import type { Invoice, Order, CreateOrder, Item, NewWidget, Receipt, Page } from "./index";
import type Ticket from "./ticket";

declare function findInvoice(id: string): Invoice;
declare function findOrders(): Order[];
declare function findReceipt(id: string): Receipt;
declare function findPage(id: string): Page<Order>;
declare function findTicket(id: string): Ticket;
declare function findCreate(): CreateOrder;

export function interfaceHome(id: string) {
  return findInvoice(id);
}

export function aliasHome(id: string) {
  return findReceipt(id);
}

export function genericHome(id: string) {
  return findPage(id);
}

export function arrayHome() {
  return findOrders();
}

export function defaultHome(id: string) {
  return findTicket(id);
}

export function jsdocHome() {
  return findCreate();
}

export function libraryHome() {
  return new Uint8Array(2);
}

export function libraryBlobHome() {
  return new Blob([]);
}

export async function stubHome() {
  return get<Invoice>("/invoices");
}

export function inlineHome(id: string) {
  return { id, ok: true };
}

export async function createItem(item: Item) {
  await fetch("/items", { method: "POST", body: JSON.stringify(item) });
}

export async function statedPage() {
  const res = await fetch("/pages"); // stated-page
  return (await res.json()) as Page<Order>;
}

const app = new App();

app.post("/orders", async (req: Request<{}, {}, CreateOrder>, res) => {
  res.json({ ok: req.body.sku });
});

app.post("/widgets", async (req, res) => {
  const body = (await req.json()) as NewWidget;
  res.json({ name: body.name });
});
`;

const H1_RESOLVERS_TS = `import type { Invoice } from "./contracts";

declare function lookup(id: string): Promise<Invoice | null>;

export const resolvers = {
  Query: {
    invoice: async (_: unknown, args: { id: string }): Promise<Invoice | null> => lookup(args.id),
  },
};
`;

describe('carrick#2148 H1: an answer reports the declared home of the type it printed', () => {
  let client: SidecarClient;
  let repoDir: string;
  const files: Record<string, string> = {};
  const sources: Record<string, string> = {
    'contracts.ts': H1_CONTRACTS_TS,
    'index.ts': H1_BARREL_TS,
    'ticket.ts': H1_TICKET_TS,
    'vendor.d.ts': H1_VENDOR_DTS,
    'routes.ts': H1_ROUTES_TS,
    'resolvers.ts': H1_RESOLVERS_TS,
  };

  before(async () => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2148-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const kitDir = path.join(repoDir, 'node_modules', 'res-kit');
    fs.mkdirSync(kitDir, { recursive: true });
    fs.writeFileSync(
      path.join(kitDir, 'package.json'),
      JSON.stringify({ name: 'res-kit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(kitDir, 'index.d.ts'), H1_KIT_DTS);
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
    for (const [name, text] of Object.entries(sources)) {
      files[name] = path.join(repoDir, 'src', name);
      fs.writeFileSync(files[name], text);
    }
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The 1-based line of the one line of `file` containing `marker`. */
  function lineIn(file: string, marker: string): number {
    const lines = sources[file].split('\n');
    const hits = lines.flatMap((line, index) => (line.includes(marker) ? [index + 1] : []));
    assert.strictEqual(hits.length, 1, `${marker} must name one line of ${file}`);
    return hits[0];
  }

  function home(symbol: string, file: string, marker: string): Home2148 {
    return { symbol, file_path: files[file], line: lineIn(file, marker) };
  }

  async function ask(file: string, request: Record<string, unknown>): Promise<Inferred> {
    const alias = String(request.alias);
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [{ file_path: files[file], ...request }],
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, `${alias} must be answered`);
    return inferred;
  }

  const returnOf = (fn: string): Promise<Inferred> =>
    ask('routes.ts', {
      line_number: lineIn('routes.ts', `function ${fn}(`),
      infer_kind: 'function_return',
      alias: `Return_${fn}`,
    });

  it('an interface reached through a barrel: the declaring file and the name line', async () => {
    const got = await returnOf('interfaceHome');
    assert.deepStrictEqual(got.type_home, home('Invoice', 'contracts.ts', 'export interface Invoice {'));
  });

  it('an object-literal alias', async () => {
    const got = await returnOf('aliasHome');
    assert.deepStrictEqual(got.type_home, home('Receipt', 'contracts.ts', 'export type Receipt = {'));
  });

  it('an instantiated generic alias is homed at its bare name', async () => {
    const got = await returnOf('genericHome');
    assert.deepStrictEqual(got.type_home, home('Page', 'contracts.ts', 'export type Page<T> = {'));
  });

  it('an array is homed at its element', async () => {
    const got = await returnOf('arrayHome');
    assert.strictEqual(got.array_depth, 1);
    assert.deepStrictEqual(got.type_home, home('Order', 'contracts.ts', 'export interface Order {'));
  });

  it('an `export default interface`', async () => {
    const got = await returnOf('defaultHome');
    assert.deepStrictEqual(got.type_home, home('Ticket', 'ticket.ts', 'export default interface Ticket {'));
  });

  it('a declaration after a JSDoc block: the line of its name, not of the comment', async () => {
    const got = await returnOf('jsdocHome');
    assert.deepStrictEqual(got.type_home, home('CreateOrder', 'contracts.ts', 'export interface CreateOrder {'));
  });

  it('a library type has none', async () => {
    const got = await returnOf('libraryHome');
    assert.match(got.type_string, /Uint8Array/);
    assert.strictEqual(got.type_home, undefined);
  });

  it('a TypeScript lib type that is no builtin anchor has none', async () => {
    const got = await returnOf('libraryBlobHome');
    assert.strictEqual(got.primary_type_symbol, 'Blob', JSON.stringify(got));
    assert.strictEqual(got.type_home, undefined);
  });

  it("a package's type declared in an ambient module block of a repo file has none", async () => {
    const got = await returnOf('stubHome');
    assert.strictEqual(got.primary_type_symbol, 'KitResponse', JSON.stringify(got));
    assert.strictEqual(got.type_home, undefined);
  });

  it('an inline object has none', async () => {
    const got = await returnOf('inlineHome');
    assert.strictEqual(got.type_home, undefined);
  });

  it('a nullable GraphQL resolver return is homed at its one named type', async () => {
    const start = H1_RESOLVERS_TS.indexOf('async (_');
    const end = H1_RESOLVERS_TS.indexOf(',\n  },');
    const got = await ask('resolvers.ts', {
      line_number: lineIn('resolvers.ts', 'async (_'),
      span_start: start,
      span_end: end,
      infer_kind: 'function_return',
      alias: 'Resolver_invoice',
    });
    assert.match(got.type_string, /null/);
    assert.deepStrictEqual(got.type_home, home('Invoice', 'contracts.ts', 'export interface Invoice {'));
  });

  it("a declared request body: the handler parameter's body type", async () => {
    const got = await ask('routes.ts', {
      line_number: lineIn('routes.ts', 'app.post("/orders"'),
      infer_kind: 'request_body',
      alias: 'Request_orders',
    });
    assert.strictEqual(got.is_explicit, true, JSON.stringify(got));
    assert.deepStrictEqual(got.type_home, home('CreateOrder', 'contracts.ts', 'export interface CreateOrder {'));
  });

  it('a request body the handler reads with a cast', async () => {
    const got = await ask('routes.ts', {
      line_number: lineIn('routes.ts', 'app.post("/widgets"'),
      infer_kind: 'request_body',
      alias: 'Request_widgets',
    });
    assert.match(got.type_string, /size: number/);
    assert.deepStrictEqual(got.type_home, home('NewWidget', 'contracts.ts', 'export interface NewWidget {'));
  });

  it('a consumer request body from a typed parameter', async () => {
    const text = 'JSON.stringify(item)';
    const start = H1_ROUTES_TS.indexOf(text);
    const got = await ask('routes.ts', {
      line_number: lineIn('routes.ts', text),
      span_start: start,
      span_end: start + text.length,
      infer_kind: 'request_body',
      alias: 'Request_item',
    });
    assert.match(got.type_string, /name: string/);
    assert.deepStrictEqual(got.type_home, home('Item', 'contracts.ts', 'export interface Item {'));
  });

  it("a stated body's root is homed at its declaration", async () => {
    const got = await ask('routes.ts', {
      line_number: lineIn('routes.ts', '// stated-page'),
      expression_text: 'fetch("/pages")',
      infer_kind: 'call_result',
      alias: 'Call_statedPage',
    });
    assert.strictEqual(got.stated_body?.root, 'Page', JSON.stringify(got));
    assert.deepStrictEqual(got.stated_body?.root_home, home('Page', 'contracts.ts', 'export type Page<T> = {'));
  });
});
