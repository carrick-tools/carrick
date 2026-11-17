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
import type { InferRequestItem } from '../src/types.js';

const FRAMEWORK_TS = `export interface JsonReply<T> { readonly body: T; readonly tag: 'json' }
export interface Ctx {
  query(key: string): string | undefined;
  json<T>(value: T): JsonReply<T>;
  notFound(): Response;
}
export type Handler = (c: Ctx) => unknown;
export interface Server { get(path: string, handler: Handler): void }
`;

const ROUTES_TS = `import type { Ctx, Server } from './framework.js';

declare function render(): string;
declare function load(): { id: number }[];

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

  it('a handler whose two returns call two helpers: the inferrer reads both, the capture is unknown', () => {
    both(read('splitPreview'));
    // No return's payload node mirrors the located one, so the capture cannot
    // join them: unknown, never the located helper's one return.
    assert.strictEqual(captured('splitPreview'), 'unknown');
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
