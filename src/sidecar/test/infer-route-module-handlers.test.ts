/**
 * carrick#807: a route its module's place in the project states.
 *
 * Such a module exports one handler per HTTP method, and nothing in it
 * registers the route: there is no call to anchor on, so the scan asks at the
 * line the export opens on. The handler takes the platform `Request` and
 * returns the platform `Response`, or a library's subclass of either.
 *
 * Three readings were wrong or missing for that shape, and none of them is
 * particular to one framework:
 *
 *  (a) A redirect whose location is a `URL` object published `string` as the
 *      route's response contract. The payload rule refuses a bare primitive
 *      (a location, a status, a body string) and admitted the same location
 *      handed over as an object, which the JSON wire print then rendered as
 *      the string its `toJSON()` returns.
 *
 *  (b) A handler passed through a wrapper (`export const POST = guarded(async
 *      (request) => ...)`) published the platform's byte stream as its REQUEST
 *      contract. The handler-parameter anchor reads the parameter's `body`
 *      member, which is a contract only where the parameter's type is written
 *      with one. The platform declares `body` as the same stream on every
 *      request, so it states nothing about this route, and it outranked the
 *      body read the model had located.
 *
 *  (c) The request side was asked only where a model located the body read. A
 *      line-only request found no registration and answered nothing, so a
 *      route no model described had no request type at all.
 *
 * The response sender and the request type come from an invented package under
 * `node_modules`, so the origin gate sees an installed dependency as it does in
 * a real tree. The platform types are TypeScript's own `dom` library.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const PACKAGE_DTS = `declare const held: unique symbol;

/** The platform response, typed by the body it was built from. */
export declare class RouteResponse<Body = unknown> extends Response {
  [held]: { body?: Body };
  constructor(body?: BodyInit | null, init?: ResponseInit);
  static json<JsonBody>(body: JsonBody, init?: ResponseInit): RouteResponse<JsonBody>;
  static redirect(url: string | URL, init?: number | ResponseInit): RouteResponse<unknown>;
}

/** The platform request with one member of its own. */
export declare class RouteRequest extends Request {
  readonly address: URL;
}

/** A request type whose body is the slot its type argument fills. */
export interface TypedRequest<Body> {
  readonly body: Body;
  readonly headers: Headers;
}

/** A schema value: it declares what it accepts and what parsing returns. */
export interface Shape<Input, Output = Input> {
  readonly _input: Input;
  readonly _output: Output;
  parse(value: unknown): Output;
}
`;

const RESPONSES_TS = `import { RouteRequest, RouteResponse } from 'routekit';

export interface Widget {
  id: string;
  name: string;
}

declare function listWidgets(): Promise<Widget[]>;
declare function findWidget(id: string): Promise<Widget | null>;

export async function GET() {
  const widgets = await listWidgets();
  return RouteResponse.json(widgets);
}

export async function HEAD() {
  const widget = await findWidget('w1');
  if (!widget) {
    return Response.json({ error: 'Not found' }, { status: 404 });
  }
  return Response.json(widget);
}

export async function PUT(request: RouteRequest) {
  return RouteResponse.redirect(new URL('/widgets', request.url));
}

export async function PATCH(request: Request) {
  return Response.redirect(new URL('/widgets', request.url));
}

class Receipt {
  constructor(private readonly pence: number) {}
  toJSON(): { total: number; currency: string } {
    return { total: this.pence / 100, currency: 'GBP' };
  }
}

export async function POST() {
  return RouteResponse.json(new Receipt(1250));
}

export async function DELETE() {
  return Response.json(new Date());
}
`;

const REQUESTS_TS = `import type { RouteRequest, TypedRequest } from 'routekit';
import { RouteResponse } from 'routekit';

export interface Renewal {
  token: string;
}

export interface Signup {
  email: string;
}

type Handler<R> = (request: R) => Promise<Response>;
declare function guarded<R>(handler: Handler<R>): Handler<R>;

export const POST = guarded(async (request: RouteRequest) => {
  const renewal = (await request.json()) as Renewal;
  return RouteResponse.json({ renewed: renewal.token.length > 0 });
});

export const PUT = guarded(async (request: TypedRequest<Signup>) => {
  return RouteResponse.json({ welcomed: request.body.email });
});

interface InviteRequest {
  body: { invitee: string };
}

export const PATCH = guarded(async (request: InviteRequest) => {
  return RouteResponse.json({ invited: request.body.invitee });
});
`;

// One handler per way of reading a body, named for the shape it holds. The
// sidecar is asked by line, so the export names carry no meaning to it.
const READS_TS = `import type { RouteRequest, Shape } from 'routekit';
import { RouteResponse } from 'routekit';

export interface NewWidget {
  name: string;
  size: number;
}

export interface Upstream {
  rate: number;
}

declare const NewWidgetShape: Shape<{ name: string; size: number }>;
declare const session: { user: unknown };

export async function castRead(request: RouteRequest) {
  const input = (await request.json()) as NewWidget;
  return RouteResponse.json({ name: input.name });
}

export async function annotatedRead(request: Request) {
  const input: NewWidget = await request.json();
  return Response.json({ name: input.name });
}

export async function validatedRead(request: Request) {
  const input = NewWidgetShape.parse(await request.json());
  return Response.json({ name: input.name });
}

export async function validatedBinding(request: Request) {
  const raw = await request.json();
  const input = NewWidgetShape.parse(raw);
  return Response.json({ name: input.name });
}

export async function destructuredParameter({ request }: { request: Request }) {
  const input = (await request.json()) as NewWidget;
  return Response.json({ name: input.name });
}

export async function untypedRead(request: Request) {
  const { name } = await request.json();
  return Response.json({ name });
}

export async function upstreamRead(request: Request) {
  const upstream = await fetch(request.url);
  const rate = (await upstream.json()) as Upstream;
  const user = session.user as { id: string };
  return Response.json({ rate: rate.rate, user: user.id });
}

export async function formRead(request: Request) {
  const form = await request.formData();
  return Response.json({ name: String(form.get('name')) });
}
`;

// The sender here comes from a package that is NOT installed, which is what a
// scan of a bare checkout sees: every call on it resolves to nothing.
const LOCATED_TS = `import { Sender } from 'absent-kit';
import { RouteResponse } from 'routekit';

export interface Part {
  sku: string;
  stock: number;
}

declare function findPart(sku: string): Promise<Part | null>;
declare function listParts(): Promise<Part[]>;
declare const summary: { parts: number };

export async function oneOrMany(request: Request) {
  const sku = new URL(request.url).searchParams.get('sku');
  if (sku) {
    return Sender.json({ one: await findPart(sku) });
  }
  return Sender.json({ many: await listParts() });
}

export async function guardedInstalled(request: Request) {
  const part = await findPart(request.url);
  if (!part) {
    return RouteResponse.json({ gone: true }, { status: 410 });
  }
  return RouteResponse.json(part);
}

export async function redirected(request: Request) {
  return RouteResponse.redirect(new URL('/parts', request.url));
}

export async function builtFirst() {
  const reply = RouteResponse.json(await listParts());
  reply.headers.set('cache-control', 'no-store');
  return reply;
}

export function sendsThroughParameter(
  _request: unknown,
  reply: { json(body: unknown): void }
) {
  reply.json(summary);
}

class Money {
  constructor(private readonly pence: number) {}
  toJSON(): string {
    return String(this.pence);
  }
}

declare const order: { pence: number; note: string };
declare function toMoney(from: { pence: number; note: string }): Money;

export function sendsBuiltValue(
  _request: unknown,
  reply: { json(body: unknown): void }
) {
  reply.json(toMoney(order));
}
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    any_provenance?: Array<{ reason: string }>;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

/** The 1-based line of the first line of `source` that holds `marker`. */
function lineOf(source: string, marker: string): number {
  const index = source.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
  return index + 1;
}

describe('carrick#807: a handler its module exports by method name', () => {
  let client: SidecarClient;
  let repoDir: string;
  const fileOf = (name: string): string => path.join(repoDir, 'src', 'routes', name);

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-807-'));
    const packageDir = path.join(repoDir, 'node_modules', 'routekit');
    fs.mkdirSync(packageDir, { recursive: true });
    fs.writeFileSync(
      path.join(packageDir, 'package.json'),
      JSON.stringify({ name: 'routekit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(packageDir, 'index.d.ts'), PACKAGE_DTS);
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
    fs.mkdirSync(path.join(repoDir, 'src', 'routes'), { recursive: true });
    fs.writeFileSync(fileOf('responses.ts'), RESPONSES_TS);
    fs.writeFileSync(fileOf('requests.ts'), REQUESTS_TS);
    fs.writeFileSync(fileOf('reads.ts'), READS_TS);
    fs.writeFileSync(fileOf('located.ts'), LOCATED_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /**
   * One request at the line the export opens on, which is the whole of what a
   * scan sends for a route no call registers. `expressionText` adds the
   * locator a model's reading of the file contributes: the text, and the line
   * it sits on (the export's own line for a text the file does not hold).
   */
  async function infer(
    file: string,
    source: string,
    exportMarker: string,
    kind: string,
    expressionText?: string
  ) {
    const alias = `Alias_${kind}_${exportMarker.replace(/\W+/g, '_')}`;
    const exportLine = lineOf(source, exportMarker);
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: fileOf(file),
          line_number: exportLine,
          infer_kind: kind,
          alias,
          ...(expressionText
            ? {
                expression_text: expressionText,
                expression_line: source.includes(expressionText)
                  ? lineOf(source, expressionText)
                  : exportLine,
              }
            : {}),
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  /** Nothing published: no answer, or an answer that is a bare top type. */
  function assertAbstains(
    inferred: { type_string: string } | undefined,
    why: string
  ): void {
    if (!inferred) return;
    assert.match(
      collapse(inferred.type_string),
      /^(any|unknown)$/,
      `${why}, got: ${inferred.type_string}`
    );
  }

  describe('the response a handler returns', () => {
    const response = (marker: string) =>
      infer('responses.ts', RESPONSES_TS, marker, 'function_return');

    it("reads the body out of a library subclass's json send", async () => {
      const inferred = await response('export async function GET');
      assert.ok(inferred, 'must resolve the body');
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; name: string; }[]');
    });

    it("reads the success body out of the platform's json send, dropping the 404 branch", async () => {
      const inferred = await response('export async function HEAD');
      assert.ok(inferred, 'must resolve the body');
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; name: string; }');
    });

    it('publishes nothing for a redirect whose location is a URL object', async () => {
      assertAbstains(
        await response('export async function PUT'),
        'a redirect sends no body, and its location is not one'
      );
    });

    it("publishes nothing for the platform's own redirect either", async () => {
      assertAbstains(
        await response('export async function PATCH'),
        'a redirect sends no body, and its location is not one'
      );
    });

    it('still reads a body whose JSON form is an object', async () => {
      // The rule is the wire form, not "has a toJSON": a value that
      // serialises to an object is a body like any other.
      const inferred = await response('export async function POST');
      assert.ok(inferred, 'a value that serialises to an object is a body');
      assert.strictEqual(
        collapse(inferred.type_string),
        '{ total: number; currency: string; }'
      );
    });

    it('publishes nothing for a body whose JSON form is a bare string', async () => {
      assertAbstains(
        await response('export async function DELETE'),
        'a bare primitive on the wire is not a contract, whatever object produced it'
      );
    });
  });

  describe("a wrapped handler's request", () => {
    const request = (marker: string, expressionText?: string) =>
      infer('requests.ts', REQUESTS_TS, marker, 'request_body', expressionText);

    it("never publishes the platform's byte stream as the contract", async () => {
      const inferred = await request('export const POST');
      assert.ok(
        !inferred || !/ReadableStream/.test(inferred.type_string),
        `the platform's own body member states nothing about this route, got: ${inferred?.type_string}`
      );
    });

    it('reads the cast on the body read from the line alone', async () => {
      const inferred = await request('export const POST');
      assert.ok(inferred, 'the handler states its body with a cast');
      assert.strictEqual(collapse(inferred.type_string), '{ token: string; }');
    });

    it('keeps the body read a model located', async () => {
      const inferred = await request('export const POST', '(await request.json()) as Renewal');
      assert.ok(inferred, 'the located read states its type');
      assert.strictEqual(collapse(inferred.type_string), '{ token: string; }');
    });

    it("still reads a body member a library's request type takes as a type argument", async () => {
      const inferred = await request('export const PUT');
      assert.ok(inferred, 'the annotation fills the body slot');
      assert.strictEqual(collapse(inferred.type_string), '{ email: string; }');
    });

    it("still reads a body member the repo's own request type declares", async () => {
      const inferred = await request('export const PATCH');
      assert.ok(inferred, 'the repo declared this body');
      assert.strictEqual(collapse(inferred.type_string), '{ invitee: string; }');
    });
  });

  describe('the request a handler reads, asked at its line alone', () => {
    const request = (marker: string) =>
      infer('reads.ts', READS_TS, marker, 'request_body');
    const NEW_WIDGET = '{ name: string; size: number; }';

    it('reads a cast on the body read', async () => {
      const inferred = await request('export async function castRead');
      assert.ok(inferred, 'the cast states the body');
      assert.strictEqual(collapse(inferred.type_string), NEW_WIDGET);
    });

    it('reads the annotation on the binding the body read initialises', async () => {
      const inferred = await request('export async function annotatedRead');
      assert.ok(inferred, 'the annotation states the body');
      assert.strictEqual(collapse(inferred.type_string), NEW_WIDGET);
    });

    it('reads the input of the schema the body read is handed to', async () => {
      const inferred = await request('export async function validatedRead');
      assert.ok(inferred, 'the schema states the body');
      assert.strictEqual(collapse(inferred.type_string), NEW_WIDGET);
    });

    it('reads the schema when the body read is bound first', async () => {
      const inferred = await request('export async function validatedBinding');
      assert.ok(inferred, 'the schema states the body');
      assert.strictEqual(collapse(inferred.type_string), NEW_WIDGET);
    });

    it('follows a request taken out of a destructured parameter', async () => {
      const inferred = await request('export async function destructuredParameter');
      assert.ok(inferred, 'the cast states the body');
      assert.strictEqual(collapse(inferred.type_string), NEW_WIDGET);
    });

    it('answers nothing for a body read nothing types', async () => {
      const inferred = await request('export async function untypedRead');
      assert.strictEqual(inferred, undefined, `got: ${inferred?.type_string}`);
    });

    it("answers nothing from a cast that is not on the request's own body", async () => {
      // A response read off an outbound call and a cast on session state are
      // both typed reads inside the handler, and neither is what a caller
      // sends.
      const inferred = await request('export async function upstreamRead');
      assert.strictEqual(inferred, undefined, `got: ${inferred?.type_string}`);
    });

    it('answers nothing for a form read, which no line states the fields of', async () => {
      const inferred = await request('export async function formRead');
      assert.strictEqual(inferred, undefined, `got: ${inferred?.type_string}`);
    });
  });

  // A model that read the file locates one response expression per route. On
  // these rows it is evidence about the handler, never the answer itself.
  describe('a response request that also located an expression', () => {
    const located = (marker: string, expressionText: string) =>
      infer('located.ts', LOCATED_TS, marker, 'response_body', expressionText);
    const lineOnly = (marker: string) =>
      infer('located.ts', LOCATED_TS, marker, 'function_return');
    const PART = '{ sku: string; stock: number; }';

    it('leaves a sender nothing resolves unread without one', async () => {
      // The control: the return walk cannot tell an unresolved sender from an
      // unresolved query, so on its own it publishes nothing.
      assertAbstains(
        await lineOnly('export async function oneOrMany'),
        'nothing says the unresolved call is a response sender'
      );
    });

    it('takes the located payload as proof of the sender and reads every body it sends', async () => {
      const inferred = await located(
        'export async function oneOrMany',
        '{ many: await listParts() }'
      );
      assert.ok(inferred, 'the located payload is an argument of a returned call');
      // Both sends, in source order; the printer puts `null` first in a union.
      assert.strictEqual(
        collapse(inferred.type_string),
        `{ one: null | ${PART}; } | { many: ${PART}[]; }`
      );
    });

    it('publishes the success body when the located expression is the error body', async () => {
      const inferred = await located('export async function guardedInstalled', '{ gone: true }');
      assert.ok(inferred, 'the handler returns a success body');
      assert.strictEqual(collapse(inferred.type_string), PART);
    });

    it('answers what the line alone answers when the located text matches nothing', async () => {
      const inferred = await located(
        'export async function guardedInstalled',
        'respondWith(somethingElse)'
      );
      assert.ok(inferred, 'the line still declares the handler');
      assert.strictEqual(collapse(inferred.type_string), PART);
    });

    it("publishes nothing when the located expression is a redirect's location", async () => {
      assertAbstains(
        await located('export async function redirected', "new URL('/parts', request.url)"),
        'the handler returns a redirect, and the located location is not a body'
      );
    });

    it('still reads a located body the handler builds before it returns', async () => {
      // What the handler returns is a variable, so the return walk reads no
      // send. The located body is not part of the return and keeps its own
      // reading.
      const inferred = await located('export async function builtFirst', 'await listParts()');
      assert.ok(inferred, 'the located body is the only statement of it');
      assert.strictEqual(collapse(inferred.type_string), `${PART}[]`);
    });

    it('still reads a located body a handler sends through a parameter', async () => {
      const inferred = await located('export function sendsThroughParameter', 'summary');
      assert.ok(inferred, 'a handler that returns nothing is read at the located send');
      assert.strictEqual(collapse(inferred.type_string), '{ parts: number; }');
    });

    it('still reads a located call that builds a value sent as a string, not what it was built from', async () => {
      // The wire-form rule is about an ARGUMENT handed to a sender. A located
      // call whose own result is the repo's value stays the payload: drilling
      // into it would publish the order it was built from.
      const inferred = await located('export function sendsBuiltValue', 'toMoney(order)');
      assert.ok(inferred, 'the located call is the payload');
      assert.strictEqual(collapse(inferred.type_string), 'string');
    });
  });
});
