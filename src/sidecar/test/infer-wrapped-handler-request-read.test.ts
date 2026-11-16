/**
 * carrick#4709: a route whose handler hands its work to a wrapper
 * (`async (request, reply) => guarded(reply, async () => { … })`) published
 * an `as const` tuple from inside a `.map` callback as its request body.
 *
 * Asked at its registration span, or after a text anchor that finds nothing,
 * the request read takes the first typed request read in the handler body
 * (`inferRequestReadFromHandler`). It took the first cast of ANY value there.
 * A cast is a request read only where it casts a value the handler reads off
 * what it was handed (its parameters, through awaits, calls made on or handed
 * such a value, and one local binding). A tuple or other value the handler
 * builds is not one, inside a wrapper's callback or not. The wrapper is still
 * looked through: a cast of the body inside its callback is read.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SCHEMA_TS = `export interface Schema<T> {
  parse(value: unknown): T;
}
export function shape<T>(): Schema<T> {
  return { parse: (value: unknown) => value as T };
}
export interface Incoming { body: unknown; params: Record<string, string> }
export interface Outgoing { code(status: number): this }
export type Handler = (request: Incoming, reply: Outgoing) => unknown;
export interface Server {
  post(path: string, handler: Handler): this;
}
`;

const ROUTES_TS = `import { shape } from './schema.js';
import type { Server, Outgoing } from './schema.js';

const OrderBody = shape<{ title: string; items: string[] }>();
const NoteBody = shape<{ text: string }>();
type Pair = [string, string];
declare function readBody(request: unknown): Promise<unknown>;

async function guarded<T>(reply: Outgoing, run: () => Promise<T>): Promise<T> {
  reply.code(200);
  return run();
}

export function orderRoutes(app: Server): void {
  app.post('/wrapped-parsed', async (request, reply) =>
    guarded(reply, async () => {
      const body = OrderBody.parse(request.body);
      const pairs = body.items.map((item) => {
        const tagged = [item, 'item'] as const;
        return tagged;
      });
      return { id: 'o-1', title: body.title, count: pairs.length };
    })
  );

  app.post('/wrapped-no-read', async (request, reply) =>
    guarded(reply, async () => {
      const labels = ['a', 'b'].map((label) => [label, 'label'] as const);
      return { count: labels.length };
    })
  );

  app.post('/wrapped-named-cast', async (request, reply) =>
    guarded(reply, async () => {
      const body = OrderBody.parse(request.body);
      const pairs = body.items.map((item) => [item, 'item'] as Pair);
      return { count: pairs.length };
    })
  );

  app.post('/wrapped-cast-body', async (request, reply) =>
    guarded(reply, async () => {
      const body = request.body as { title: string };
      const tags = [body.title, 'tag'] as const;
      return { tags };
    })
  );

  app.post('/direct-mapped', async (request) => {
    const body = OrderBody.parse(request.body);
    const pairs = body.items.map((item) => [item, 'item'] as const);
    return { count: pairs.length };
  });

  app.post('/cast-of-a-parse', async (request) => {
    const order = JSON.parse(request.body as string) as { sku: string };
    return { saved: order.sku };
  });

  app.post('/cast-of-a-schema-parse', async (request) => {
    const note = NoteBody.parse(request.body) as { text: string; pinned: boolean };
    return { saved: note.text };
  });

  app.post('/cast-of-a-helper-read', async (request) => {
    const sized = (await readBody(request)) as { size: number };
    return { saved: sized.size };
  });

  app.post('/cast-of-a-local', async (request) => {
    const raw = request.body;
    const kinded = raw as { kind: string };
    return { saved: kinded.kind };
  });

  app.post('/cast-of-a-destructured-local', async (request) => {
    const { body } = request;
    const flagged = body as { flag: boolean };
    return { saved: flagged.flag };
  });

  app.post('/direct-typed-param', async (request: { body: { note: string } }) => {
    return { saved: request.body.note };
  });

  app.post('/direct-parsed', async (request) => {
    const body = NoteBody.parse(request.body);
    return { saved: body.text };
  });
}
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; reason: string }>;
  }>;
}

const flat = (text: string | undefined): string | undefined => text?.replace(/\s+/g, ' ').trim();

describe('carrick#4709: a wrapped handler read at its registration never publishes an inner expression', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;
  let n = 0;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4709-'));
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
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(repoDir, 'src', 'schema.ts'), SCHEMA_TS);
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

  /** The registration call for `route`, as the scanner sends its span. */
  const registration = (route: string): { span_start: number; span_end: number; line: number } => {
    const start = ROUTES_TS.indexOf(`app.post('${route}'`);
    assert.ok(start >= 0, `fixture must register ${route}`);
    let depth = 0;
    for (let i = ROUTES_TS.indexOf('(', start); i < ROUTES_TS.length; i++) {
      if (ROUTES_TS[i] === '(') depth++;
      else if (ROUTES_TS[i] === ')' && --depth === 0) {
        return { span_start: start, span_end: i + 1, line: ROUTES_TS.slice(0, start).split('\n').length };
      }
    }
    assert.fail('unbalanced');
  };

  /**
   * The request body asked the two ways the scanner asks a row with no request
   * anchor: the registration span, or a model text that is not in the file.
   */
  const ask = async (route: string, how: 'span' | 'missed_text') => {
    const site = registration(route);
    const alias = `Request_${n++}`;
    const locator =
      how === 'span'
        ? { span_start: site.span_start, span_end: site.span_end }
        : { expression_text: 'payloadThatIsNotInTheFile', expression_line: site.line };
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        { file_path: routesPath, line_number: site.line, infer_kind: 'request_body', alias, ...locator },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  for (const how of ['span', 'missed_text'] as const) {
    it(`(1) a wrapped handler that parses its body: never the inner tuple (${how})`, async () => {
      const inferred = await ask('/wrapped-parsed', how);
      assert.doesNotMatch(flat(inferred?.type_string) ?? '', /readonly \[/);
      assert.strictEqual(inferred, undefined, `unresolved, not ${flat(inferred?.type_string)}`);
    });

    it(`(2) a wrapped handler that reads no body: no type (${how})`, async () => {
      const inferred = await ask('/wrapped-no-read', how);
      assert.strictEqual(inferred, undefined, `unresolved, not ${flat(inferred?.type_string)}`);
    });

    it(`(1b) a wrapped handler that casts another value to a named type: no type (${how})`, async () => {
      const inferred = await ask('/wrapped-named-cast', how);
      assert.strictEqual(inferred, undefined, `unresolved, not ${flat(inferred?.type_string)}`);
    });

    it(`(1c) the wrapper is looked through: a cast of the body inside its callback is read (${how})`, async () => {
      assert.strictEqual(flat((await ask('/wrapped-cast-body', how))?.type_string), '{ title: string; }');
    });

    it(`(1d) a direct handler with the same inner tuple: no type (${how})`, async () => {
      const inferred = await ask('/direct-mapped', how);
      assert.strictEqual(inferred, undefined, `unresolved, not ${flat(inferred?.type_string)}`);
    });

    for (const [route, expected] of [
      ['/cast-of-a-parse', '{ sku: string; }'],
      ['/cast-of-a-schema-parse', '{ text: string; pinned: boolean; }'],
      ['/cast-of-a-helper-read', '{ size: number; }'],
      ['/cast-of-a-local', '{ kind: string; }'],
      ['/cast-of-a-destructured-local', '{ flag: boolean; }'],
    ] as const) {
      it(`(3c) control: a cast of what the handler reads keeps today's reading, ${route} (${how})`, async () => {
        assert.strictEqual(flat((await ask(route, how))?.type_string), expected);
      });
    }

    it(`(3a) control: a direct handler's typed body parameter is kept (${how})`, async () => {
      assert.strictEqual(flat((await ask('/direct-typed-param', how))?.type_string), '{ note: string; }');
    });

    it(`(3b) control: a direct handler that parses its body stays unresolved (${how})`, async () => {
      assert.strictEqual(await ask('/direct-parsed', how), undefined);
    });
  }
});
