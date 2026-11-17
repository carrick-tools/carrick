/**
 * carrick#4778: a route's response is read from one return of the function
 * that answers for it. When that function has several returns that answer
 * with different types, no one of them is its type: the answer is all of
 * them (the capture's union of each return's own payload, the inferrer's
 * whole-function return), or unknown. Same rule for the capture
 * (`captureStub`) and the inferrer (`TypeInferrer.infer`): `anchorInHandler`
 * (capture/site-node.ts) names the function, both read it.
 *
 * Seeds (invented names): a helper with a preview branch and a normal branch
 * (the issue's shape); the same two returns inline in the handler; a handler
 * whose two returns call two helpers; and controls: one return, two returns
 * of one type, and a helper with an early body-less framework reply beside a
 * JSON reply (the JSON payload is the answer, the reply carries none).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { captureStub } from '../src/capture/index.js';
import { TypeInferrer } from '../src/type-inferrer.js';
import type { InferRequestItem, InferredType } from '../src/types.js';

const FRAMEWORK_TS = `export interface JsonReply<T> { readonly body: T; readonly tag: 'json' }
export interface Ctx {
  query(key: string): string | undefined;
  json<T>(value: T, status?: number): JsonReply<T>;
  notFound(): Response;
}
export interface Reply {
  status(code: number): Reply;
  send(payload: unknown): Response;
}
export interface Delivered<T> { readonly payload: T; readonly tag: 'delivered' }
export interface Courier {
  status(code: number): Courier;
  deliver<T>(payload: T): Delivered<T>;
}
export type Handler = (c: Ctx) => unknown;
export interface Server {
  get(path: string, handler: Handler): void;
  on(path: string, handler: (courier: Courier) => unknown): void;
}
`;

const ROUTES_TS = `import type { Courier, Ctx, JsonReply, Reply, Server } from './framework.js';

declare function render(): string;
declare function load(): { id: number }[];
declare function rows(): { row: string }[];
declare function flag(): boolean;

function show(c: Ctx) {
  if (c.query('preview')) return c.json({ preview: true, html: render() });
  return c.json({ order: load(), total: 3 });
}

function single(c: Ctx) {
  return c.json({ solo: load() });
}

function sameType(c: Ctx) {
  if (c.query('x')) return c.json({ count: 1 });
  return c.json({ count: 2 });
}

function early(c: Ctx) {
  if (!c.query('id')) return c.notFound();
  return c.json({ found: true, label: render() });
}

function lookup(c: Ctx) {
  if (!c.query('id')) return c.json({ error: 'not found' }, 404);
  return c.json({ order: load(), total: 3 });
}

function chained(reply: Reply, id: string | undefined) {
  if (!id) return reply.status(404).send({ error: 'missing' });
  return reply.send({ order: load(), total: 3 });
}

function guarded(c: Ctx) {
  if (!c.query('id')) return c.json({ error: 'not found' }, 404);
  if (c.query('preview')) return c.json({ preview: true, html: render() });
  return c.json({ order: load(), total: 3 });
}

function twice(c: Ctx) {
  if (!c.query('id')) return c.json({ error: 'gone' }, 410);
  if (c.query('fresh')) return c.json({ order: load(), total: 3 });
  return c.json({ order: load(), total: 3 });
}

function stated(c: Ctx): JsonReply<{ error: string }> | JsonReply<{ order: { id: number }[]; total: number }> {
  if (!c.query('id')) return c.json({ error: 'not found' }, 404);
  return c.json({ order: load(), total: 3 });
}

function create(c: Ctx) {
  if (c.query('draft')) return c.json({ draft: true, html: render() }, 201);
  return c.json({ order: load(), total: 3 }, 200);
}

function moved(c: Ctx) {
  if (c.query('old')) return c.json({ location: '/orders' }, 302);
  return c.json({ order: load(), total: 3 });
}

function relay(c: Ctx, code: number) {
  if (c.query('upstream')) return c.json({ problem: render() }, code);
  return c.json({ order: load(), total: 3 });
}

function page(c: Ctx) {
  if (c.query('raw')) return rows().slice(0, 500);
  return c.json({ order: load(), total: 3 });
}

function deliverPart(courier: Courier) {
  return courier.deliver({ order: load(), total: 3 });
}

function previewPart(c: Ctx) {
  return c.json({ preview: true, html: render() });
}

function orderPart(c: Ctx) {
  return c.json({ order: load(), total: 3 });
}

export function routes(app: Server) {
  app.get('/orders/:id', (c) => show(c));

  app.get('/inline/:id', (c) => {
    if (c.query('preview')) return c.json({ preview: true, html: render() });
    return c.json({ order: load(), total: 3 });
  });

  app.get('/split/:id', (c) => {
    if (c.query('preview')) return previewPart(c);
    return orderPart(c);
  });

  app.get('/lookup', (c) => lookup(c));

  app.get('/inline-error', (c) => {
    if (!c.query('id')) return c.json({ error: 'not found' }, 404);
    return c.json({ order: load(), total: 3 });
  });

  app.get('/chained/:id', (c) => chained(c as unknown as Reply, c.query('id')));

  app.get('/guarded', (c) => guarded(c));

  app.get('/twice', (c) => twice(c));

  app.get('/guard-then-helper', (c) => {
    if (!c.query('id')) return c.json({ error: 'not found' }, 404);
    return orderPart(c);
  });

  app.get('/stated', (c) => stated(c));

  app.get('/helper-then-error', (c) => {
    if (c.query('id')) return orderPart(c);
    return c.json({ error: 'not found' }, 404);
  });

  app.on('/chain-mirror', (courier) => {
    if (flag()) return deliverPart(courier);
    return courier.status(404).deliver({ error: 'missing' });
  });

  app.get('/create', (c) => create(c));

  app.get('/moved', (c) => moved(c));

  app.get('/relay', (c) => relay(c, 502));

  app.get('/page', (c) => page(c));

  app.get('/solo', (c) => single(c));

  app.get('/same', (c) => sameType(c));

  app.get('/early', (c) => early(c));
}
`;

const LINES = ROUTES_TS.split('\n');
function lineWith(fragment: string, from = 1): number {
  const idx = LINES.findIndex((l, i) => i + 1 >= from && l.includes(fragment));
  assert.ok(idx >= 0, `fixture must contain ${fragment}`);
  return idx + 1;
}
function handler(opening: string, oneLine = false): { start_line: number; end_line: number } {
  const start = lineWith(opening);
  return { start_line: start, end_line: oneLine ? start : lineWith('  });', start) };
}

interface Ask {
  kind: 'response_body' | 'function_return';
  text: string;
  line: number;
  handler_lines: { start_line: number; end_line: number };
}

const PREVIEW = '{ preview: true, html: render() }';
const ORDER = '{ order: load(), total: 3 }';

const ASKS: Record<string, Ask> = {
  // (a) the issue's shape: the helper's first return, then its second.
  helperPreview: {
    kind: 'response_body',
    text: PREVIEW,
    line: lineWith('return c.json({ preview: true', lineWith('function show')),
    handler_lines: handler("app.get('/orders/:id'", true),
  },
  helperOrder: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load()', lineWith('function show')),
    handler_lines: handler("app.get('/orders/:id'", true),
  },
  helperCall: {
    kind: 'function_return',
    text: `c.json(${PREVIEW})`,
    line: lineWith('return c.json({ preview: true', lineWith('function show')),
    handler_lines: handler("app.get('/orders/:id'", true),
  },
  // (b) the same two returns written in the handler.
  inlinePreview: {
    kind: 'response_body',
    text: PREVIEW,
    line: lineWith('return c.json({ preview: true', lineWith("app.get('/inline")),
    handler_lines: handler("app.get('/inline/:id'"),
  },
  inlineOrder: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load()', lineWith("app.get('/inline")),
    handler_lines: handler("app.get('/inline/:id'"),
  },
  // The handler has the two returns; each calls a helper with one.
  splitPreview: {
    kind: 'response_body',
    text: PREVIEW,
    line: lineWith('return c.json({ preview: true', lineWith('function previewPart')),
    handler_lines: handler("app.get('/split/:id'"),
  },
  // An error reply with a body beside the success reply: the success body alone.
  lookupSuccess: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load(), total: 3 });', lineWith('function lookup')),
    handler_lines: handler("app.get('/lookup'", true),
  },
  lookupError: {
    kind: 'response_body',
    text: "{ error: 'not found' }",
    line: lineWith("return c.json({ error: 'not found' }, 404)", lineWith('function lookup')),
    handler_lines: handler("app.get('/lookup'", true),
  },
  inlineErrorSuccess: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load(), total: 3 });', lineWith("app.get('/inline-error'")),
    handler_lines: handler("app.get('/inline-error'"),
  },
  chainedSuccess: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return reply.send({ order'),
    handler_lines: handler("app.get('/chained/:id'", true),
  },
  chainedError: {
    kind: 'response_body',
    text: "{ error: 'missing' }",
    line: lineWith("return reply.status(404).send({ error: 'missing' })"),
    handler_lines: handler("app.get('/chained/:id'", true),
  },
  // An error reply beside two differing success replies: the two, not the error.
  guardedOrder: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load()', lineWith('function guarded')),
    handler_lines: handler("app.get('/guarded'", true),
  },
  // Located on the error reply, two returns of one type stay: that type.
  twiceError: {
    kind: 'response_body',
    text: "{ error: 'gone' }",
    line: lineWith("return c.json({ error: 'gone' }, 410)"),
    handler_lines: handler("app.get('/twice'", true),
  },
  // carrick#4796: the handler's inline error reply beside a return of a
  // helper's reply. The error reply calls the located send's own callee, so
  // it leaves; one return stays and the row reads as on main.
  guardThenHelper: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load()', lineWith('function orderPart')),
    handler_lines: handler("app.get('/guard-then-helper'"),
  },
  // Its mirror: the helper first, the inline error second.
  helperThenError: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load()', lineWith('function orderPart')),
    handler_lines: handler("app.get('/helper-then-error'"),
  },
  // The mirror again, with the error status stated by the reply chain.
  chainMirror: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return courier.deliver({ order', lineWith('function deliverPart')),
    handler_lines: handler("app.on('/chain-mirror'"),
  },
  // An annotated helper, located on its error reply.
  statedError: {
    kind: 'response_body',
    text: "{ error: 'not found' }",
    line: lineWith("return c.json({ error: 'not found' }, 404)", lineWith('function stated')),
    handler_lines: handler("app.get('/stated'", true),
  },
  // Two success statuses with different bodies: both.
  createDraft: {
    kind: 'response_body',
    text: '{ draft: true, html: render() }',
    line: lineWith('return c.json({ draft: true', lineWith('function create')),
    handler_lines: handler("app.get('/create'", true),
  },
  // A redirect beside a JSON reply: the JSON reply alone.
  movedOrder: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load()', lineWith('function moved')),
    handler_lines: handler("app.get('/moved'", true),
  },
  // A status held in a variable is `variable`: kept, so both (fail open).
  relayOrder: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load()', lineWith('function relay')),
    handler_lines: handler("app.get('/relay'", true),
  },
  // The same, located on the whole returned call.
  pageCall: {
    kind: 'function_return',
    text: `c.json(${ORDER})`,
    line: lineWith('return c.json({ order: load()', lineWith('function page')),
    handler_lines: handler("app.get('/page'", true),
  },
  // A return that mirrors no send is never judged by status.
  pageOrder: {
    kind: 'response_body',
    text: ORDER,
    line: lineWith('return c.json({ order: load()', lineWith('function page')),
    handler_lines: handler("app.get('/page'", true),
  },
  // (c) controls.
  solo: {
    kind: 'response_body',
    text: '{ solo: load() }',
    line: lineWith('return c.json({ solo'),
    handler_lines: handler("app.get('/solo'", true),
  },
  same: {
    kind: 'response_body',
    text: '{ count: 1 }',
    line: lineWith('return c.json({ count: 1 })'),
    handler_lines: handler("app.get('/same'", true),
  },
  early: {
    kind: 'response_body',
    text: '{ found: true, label: render() }',
    line: lineWith('return c.json({ found: true'),
    handler_lines: handler("app.get('/early'", true),
  },
};

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#4778: a function with several differing returns is not one return', () => {
  let repoDir: string;
  let outRoot: string;
  let surface: string;
  let inferred: Map<string, string>;
  let configured: Map<string, string>;
  let rows: Map<string, InferredType>;

  before(() => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4778-')));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4778-stub-'));
    fs.mkdirSync(path.join(repoDir, 'src'));
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
    fs.writeFileSync(path.join(repoDir, 'src', 'framework.ts'), FRAMEWORK_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'routes.ts'), ROUTES_TS);

    const result = captureStub({
      repoRoot: repoDir,
      serviceName: 'several-returns-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: Object.entries(ASKS).map(([name, ask]) => ({
        kind: 'infer' as const,
        alias: `Capture_${name}`,
        source_file: 'src/routes.ts',
        anchor_origin: 'deterministic-infer' as const,
        expression_text: ask.text,
        line_number: ask.line,
        infer_kind: ask.kind,
        handler_lines: ask.handler_lines,
      })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');

    const project = new Project({ tsConfigFilePath: path.join(repoDir, 'tsconfig.json') });
    const inferrer = new TypeInferrer({ project, repoRoot: repoDir });
    const requests: InferRequestItem[] = Object.entries(ASKS).map(([name, ask]) => ({
      file_path: path.join(repoDir, 'src', 'routes.ts'),
      line_number: ask.line,
      expression_text: ask.text,
      expression_line: ask.line,
      infer_kind: ask.kind,
      alias: `Infer_${name}`,
      handler_lines: ask.handler_lines,
    }));
    const res = inferrer.infer(requests);
    inferred = new Map((res.inferred_types ?? []).map((t) => [t.alias, collapse(t.type_string)]));
    rows = new Map((res.inferred_types ?? []).map((t) => [t.alias, t]));
    // The scan's extraction config names the framework's reply type; each
    // kept return's reply is unwrapped to its payload.
    const configuredInferrer = new TypeInferrer({
      project: new Project({ tsConfigFilePath: path.join(repoDir, 'tsconfig.json') }),
      repoRoot: repoDir,
    });
    const withRule = configuredInferrer.infer(requests, {
      rules: [{ wrapperSymbols: ['JsonReply'], payloadPropertyPath: ['body'] }],
    });
    configured = new Map(
      (withRule.inferred_types ?? []).map((t) => [t.alias, collapse(t.type_string)])
    );
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  function captured(name: string): string {
    const alias = `Capture_${name}`;
    const m = surface.match(
      new RegExp(`export type ${alias} = ([\\s\\S]*?);(?=\\s*(?:export type |$))`)
    );
    assert.ok(m, `no surface line for ${alias}:\n${surface}`);
    return collapse(m![1]);
  }
  const read = (name: string): string => {
    const text = inferred.get(`Infer_${name}`);
    assert.ok(text !== undefined, `${name}: the inferrer read nothing`);
    return text;
  };
  const both = (text: string): void => {
    assert.match(text, /preview/, text);
    assert.match(text, /order/, text);
  };

  it('(a) a helper with a preview and a normal return is both, whichever return the locator found', () => {
    for (const name of ['helperPreview', 'helperOrder', 'helperCall']) {
      both(captured(name));
      both(read(name));
    }
  });

  it('(b) the same two returns written in the handler are both', () => {
    for (const name of ['inlinePreview', 'inlineOrder']) {
      both(captured(name));
      both(read(name));
    }
  });

  it('a handler whose two returns call two helpers: unknown on both paths', () => {
    // No return's payload node mirrors the located one, so neither path can
    // read each return as a lone return is read: unknown, never the located
    // helper's one return and never the replies that carry the payloads.
    assert.strictEqual(captured('splitPreview'), 'unknown');
    assert.strictEqual(inferred.get('Infer_splitPreview'), undefined);
  });

  it('with no wrapper rule, each return is read as a lone return is: the payloads, not the replies', () => {
    for (const name of ['helperPreview', 'helperOrder', 'inlinePreview', 'guardedOrder', 'createDraft', 'relayOrder']) {
      for (const text of [captured(name), read(name)]) {
        assert.match(text, /order/, `${name}: ${text}`);
        assert.doesNotMatch(text, /body:|tag:|JsonReply/, `${name}: ${text}`);
      }
    }
  });

  const successAlone = (name: string): void => {
    for (const text of [captured(name), read(name)]) {
      assert.match(text, /order/, `${name}: ${text}`);
      assert.doesNotMatch(text, /error|location/, `${name}: ${text}`);
    }
  };

  it('an error reply beside the success reply, in a helper: the success payload alone', () => {
    successAlone('lookupSuccess');
  });

  it('the same, located on the error reply: the success payload alone', () => {
    successAlone('lookupError');
  });

  it('an error reply beside the success reply, inline: the success payload alone', () => {
    successAlone('inlineErrorSuccess');
  });

  it('an error status stated by the reply chain: the success payload alone', () => {
    successAlone('chainedSuccess');
  });

  it('an error reply beside two differing success replies: the union of the two', () => {
    for (const text of [captured('guardedOrder'), read('guardedOrder')]) {
      both(text);
      assert.doesNotMatch(text, /error/, text);
    }
  });

  it('the same, located on the chained error reply: never the error payload', () => {
    // The inferrer reads the success reply alone. The capture's builder-chain
    // rule (a located node inside `a.b(x).c(y)`) demotes it first: unknown.
    assert.match(read('chainedError'), /order/);
    assert.doesNotMatch(read('chainedError'), /error/);
    assert.strictEqual(captured('chainedError'), 'unknown');
  });

  it('a redirect beside a JSON reply: the JSON reply alone', () => {
    successAlone('movedOrder');
  });

  it('a 201 beside a 200 with different bodies: both', () => {
    for (const text of [captured('createDraft'), read('createDraft')]) {
      assert.match(text, /draft/, text);
      assert.match(text, /order/, text);
    }
  });

  it('a status held in a variable is kept: both', () => {
    for (const text of [captured('relayOrder'), read('relayOrder')]) {
      assert.match(text, /problem/, text);
      assert.match(text, /order/, text);
    }
  });

  it('a return that mirrors no send is not judged by its arguments: never dropped', () => {
    // `rows().slice(0, 500)` is no reply, so its `500` is no status. Located
    // on the order (the argument), neither path can read that return's
    // payload, so the whole is unknown; located on the whole call, both read
    // both calls. Never the order alone.
    assert.strictEqual(captured('pageOrder'), 'unknown');
    assert.strictEqual(inferred.get('Infer_pageOrder'), undefined);
    assert.match(read('pageCall'), /row/);
    assert.match(read('pageCall'), /order/);
    assert.match(captured('pageCall'), /row/);
  });

  it('located on the error reply beside two returns of one type: that type alone', () => {
    for (const text of [captured('twiceError'), read('twiceError')]) {
      assert.strictEqual(text.match(/order:/g)?.length, 1, text);
      assert.doesNotMatch(text, /error/, text);
    }
  });

  it('an annotated helper: the kept return is read, not the annotation that names the error', () => {
    successAlone('statedError');
    const row = rows.get('Infer_statedError');
    assert.strictEqual(row?.is_explicit, false, JSON.stringify(row));
  });

  it('carrick#4796: an inline error reply beside a helper reply leaves; the helper success type, as on main', () => {
    successAlone('guardThenHelper');
  });

  it('carrick#4796 mirrored: the helper reply first, the inline error second', () => {
    successAlone('helperThenError');
  });

  it('carrick#4796 mirrored, the error status stated by the reply chain', () => {
    successAlone('chainMirror');
  });

  it('with a wrapper rule for the reply type, the kept returns alone are unwrapped', () => {
    for (const name of ['lookupError', 'guardedOrder', 'twiceError']) {
      const text = configured.get(`Infer_${name}`) ?? '';
      assert.match(text, /order/, `${name}: ${text}`);
      assert.doesNotMatch(text, /error|tag/, `${name}: ${text}`);
    }
    assert.match(configured.get('Infer_guardedOrder') ?? '', /preview/);
    // Control: no return left, the rule unwraps both as before.
    assert.match(configured.get('Infer_helperOrder') ?? '', /preview/);
    assert.match(configured.get('Infer_helperOrder') ?? '', /order/);
  });

  it('(c) one return is that return, alone', () => {
    assert.match(captured('solo'), /solo/);
    assert.match(read('solo'), /solo/);
  });

  it('(c) two returns of one type are that type, as before', () => {
    // One member, not a union of two equal ones: the rule left the row alone.
    for (const text of [captured('same'), read('same')]) {
      assert.strictEqual(text.match(/count:/g)?.length, 1, text);
    }
  });

  it('(c) a body-less framework reply beside a JSON reply: the JSON payload is the answer', () => {
    for (const text of [captured('early'), read('early')]) {
      assert.match(text, /found/, text);
      assert.doesNotMatch(text, /Response/, text);
    }
  });
});
