/**
 * carrick#1935: the inferrer's locators read a per-file node index, walked
 * once per file, and name exactly the node the per-item walk named.
 *
 * `registrationAtLine`, `findNodeAtSpan`, `findNodeByText`,
 * `findCallExpressionByText` and `findCallExpressionAtSpan` each called
 * `getDescendants()` (or `getDescendantsOfKind`) on the whole file once per
 * item: on a generated client of 8,000+ lines that was 71% of an infer
 * request.
 *
 * - Test 1 keeps the five walks as they stood (`reference*` below, fed the
 *   file's `getDescendants()`) and asks both of every line, every node's span
 *   (exact, shifted one byte as an SWC span is, widened, empty) and every
 *   node's text at its line and at the window's edges. Each pair must name
 *   the same wrapper object.
 * - Test 2 proves the index's walk is ts-morph's: the same nodes, in the same
 *   order, for both the whole walk and the call expressions.
 * - Test 3 counts the full-file walks of one infer batch: one per file.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Node, Project, SyntaxKind, type CallExpression, type SourceFile } from 'ts-morph';
import { startLineOf } from '../src/line-index.js';
import { callsOf, descendantsOf, nodeIndexBuilds } from '../src/node-index.js';
import { TypeInferrer } from '../src/type-inferrer.js';
import type { InferRequestItem } from '../src/types.js';

const CLIENT_FUNCTIONS = 420;
const ROUTES = 220;

/** A generated client: a result-shaped transport and N exported functions. */
function clientSource(n: number): string {
  const lines = [
    `type Result<T, E> = { data: T; error: undefined } | { data: undefined; error: E };`,
    `declare const client: {`,
    `  get<R, E, T extends boolean = false>(o: object): Promise<Result<R, E>>;`,
    `  post<R, E, T extends boolean = false>(o: object): Promise<Result<R, E>>;`,
    `};`,
    `type Options<D, T extends boolean> = { client?: typeof client; body?: D; throwOnError?: T };`,
  ];
  for (let i = 0; i < n; i++) {
    lines.push(
      `/** Reads thing ${i}. */`,
      `export type GetThing${i}Data = { id: number };`,
      `export type GetThing${i}Responses = { 200: { name: string; n${i}: number } };`,
      `export type GetThing${i}Errors = { 404: { message: string } };`,
      `export const getThing${i} = <ThrowOnError extends boolean = false>(options: Options<GetThing${i}Data, ThrowOnError>) =>`,
      `    (options.client ?? client).get<GetThing${i}Responses, GetThing${i}Errors, ThrowOnError>({`,
      `        security: [{ scheme: 'bearer', type: 'http' }],`,
      `        url: '/things/{id}',`,
      `        ...options`,
      `    });`,
      `// (options.client ?? client).post({ url: '/things/${i}' })`,
      `export const postThing${i} = <ThrowOnError extends boolean = false>(options: Options<{ name: string }, ThrowOnError>) =>`,
      `    (options.client ?? client).post<GetThing${i}Responses, GetThing${i}Errors, ThrowOnError>({`,
      `        url: '/things',`,
      `        body: { name: 'thing${i}', n: ${i} },`,
      `        ...options`,
      `    });`,
      `export function listThing${i}() { return client.get<GetThing${i}Responses, GetThing${i}Errors>({ url: '/things' }) }`,
      ``,
      ``
    );
  }
  return lines.join('\n') + '\n';
}

/**
 * A route module: handlers declared away from their registrations, inline
 * handlers, object-literal registrations, a call and an object literal on one
 * line, and statements with no semicolon (the statement spans exactly its
 * call: the carrick#1166 tie).
 */
function routesSource(n: number): string {
  const lines = [
    `type Req = { body: { id: number } };`,
    `type Res = { json(v: unknown): void };`,
    `type Handler = (req: Req, res: Res) => void;`,
    `declare const app: { get(p: string, h: Handler): void; post(p: string, ...h: Handler[]): void };`,
    `declare const router: { route(r: { path: string; handler: Handler }): void };`,
    `const table: { method: string; path: string; handler: Handler }[] = [];`,
  ];
  for (let i = 0; i < n; i++) {
    lines.push(
      `function handler${i}(req: Req, res: Res) {`,
      `  res.json({ id: req.body.id, n: ${i} });`,
      `}`
    );
  }
  for (let i = 0; i < n; i++) {
    switch (i % 5) {
      case 0:
        lines.push(`app.get('/r${i}', handler${i})`);
        break;
      case 1:
        lines.push(`app.post('/r${i}', (req, res) => { res.json({ inline: ${i} }) });`);
        break;
      case 2:
        lines.push(`router.route({ path: '/r${i}', handler: handler${i} });`);
        break;
      case 3:
        lines.push(`table.push({ method: 'GET', path: '/r${i}', handler: handler${i} })`);
        break;
      default:
        lines.push(
          `app.post(`,
          `  '/r${i}',`,
          `  (req, res) => res.json({ multi: ${i}, id: req.body.id })`,
          `);`
        );
    }
  }
  return lines.join('\n') + '\n';
}

/** Small shapes: doc comments, comment lines, empty statements, CRLF. */
const SHAPES = [
  `/**`,
  ` * A documented function.`,
  ` * @param a the input`,
  ` */`,
  `export function documented(a: number) { return { a } }`,
  `// a comment statement`,
  `;;`,
  `const x = [1, 2, 3].map((v) => ({ v })).filter(Boolean)`,
  `export default { handler: documented, nested: { call: documented(1) } };`,
  `\r\nconst y = documented(2);\r\n`,
].join('\n');

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
  include: ['src/**/*.ts'],
});

// ---------------------------------------------------------------------------
// The walks as they stood before carrick#1935, verbatim but for `nodes`, which
// is the file's `getDescendants()` they each recomputed, and `inferrer`,
// whose shared helpers they called through `this`.
// ---------------------------------------------------------------------------

function referenceRegistrationAtLine(inferrer: any, nodes: Node[], line: number) {
  for (const node of nodes) {
    if (
      (Node.isCallExpression(node) || Node.isObjectLiteralExpression(node)) &&
      startLineOf(node) === line
    ) {
      const handler = inferrer.resolveRegisteredHandler(node);
      if (handler) {
        return { registration: node, handler };
      }
    }
  }
  return undefined;
}

function referenceFindNodeByText(
  inferrer: any,
  nodes: Node[],
  expressionText: string,
  lineNumber?: number,
  searchRadius = 5
): Node | undefined {
  const allNodes = nodes.filter((node) => {
    if (Node.isSourceFile(node)) return false;
    if (node.getKind() === SyntaxKind.SyntaxList) return false;
    return true;
  });
  return inferrer.matchByText(allNodes, expressionText, lineNumber, searchRadius);
}

function referenceFindCallExpressionByText(
  inferrer: any,
  calls: CallExpression[],
  expressionText: string,
  lineNumber?: number,
  searchRadius = 5
): CallExpression | undefined {
  return inferrer.matchByText(calls, expressionText, lineNumber, searchRadius);
}

function referenceFindNodeAtSpan(nodes: Node[], spanStart: number, spanEnd: number): Node | undefined {
  const containing = nodes.filter((node) => {
    if (Node.isSourceFile(node)) return false;
    if (node.getKind() === SyntaxKind.SyntaxList) return false;
    const start = node.getStart();
    const end = node.getEnd();
    return start <= spanStart && spanEnd <= end;
  });
  if (containing.length === 0) return undefined;
  return containing.reduce((best, current) => {
    const bestRange = best.getEnd() - best.getStart();
    const currentRange = current.getEnd() - current.getStart();
    const bestDelta = Math.abs(bestRange - (spanEnd - spanStart));
    const currentDelta = Math.abs(currentRange - (spanEnd - spanStart));
    if (currentDelta !== bestDelta) {
      return currentDelta < bestDelta ? current : best;
    }
    return current.getStart() >= best.getStart() && current.getEnd() <= best.getEnd()
      ? current
      : best;
  });
}

function referenceFindCallExpressionAtSpan(
  calls: CallExpression[],
  spanStart: number,
  spanEnd: number
): CallExpression | undefined {
  const SPAN_SLACK = 2;
  const candidates = calls.filter((expr) => {
    const start = expr.getStart();
    const end = expr.getEnd();
    return start - SPAN_SLACK <= spanStart && spanEnd <= end + SPAN_SLACK;
  });
  if (candidates.length === 0) return undefined;
  return candidates.reduce((best, current) => {
    const bestRange = best.getEnd() - best.getStart();
    const currentRange = current.getEnd() - current.getStart();
    const bestDelta = Math.abs(bestRange - (spanEnd - spanStart));
    const currentDelta = Math.abs(currentRange - (spanEnd - spanStart));
    return currentDelta < bestDelta ? current : best;
  });
}

const identity = (n: Node | undefined) =>
  n ? `${n.getKindName()}@${n.getStart()}-${n.getEnd()}` : 'none';

/** Every k-th element, so the large files stay quick against the walk. */
function sample<T>(items: T[], max: number): T[] {
  if (items.length <= max) return items;
  const step = items.length / max;
  const out: T[] = [];
  for (let k = 0; k < items.length; k += step) out.push(items[Math.floor(k)]);
  return out;
}

describe('per-file node index for the infer locators (carrick#1935)', () => {
  let root: string;
  let project: Project;
  const files: Record<string, string> = {};

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'node-index-'));
    fs.mkdirSync(path.join(root, 'src'));
    fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
    files.client = path.join(root, 'src/client.gen.ts');
    files.routes = path.join(root, 'src/routes.ts');
    files.shapes = path.join(root, 'src/shapes.ts');
    fs.writeFileSync(files.client, clientSource(CLIENT_FUNCTIONS));
    fs.writeFileSync(files.routes, routesSource(ROUTES));
    fs.writeFileSync(files.shapes, SHAPES);
    project = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('the fixtures are the sizes the ticket names', () => {
    const client = project.getSourceFileOrThrow(files.client);
    assert.ok(client.getEndLineNumber() > 8000, `client has ${client.getEndLineNumber()} lines`);
    assert.ok(client.getFunctions().length + client.getVariableDeclarations().length >= 400);
    const routes = project.getSourceFileOrThrow(files.routes);
    const inf = new TypeInferrer({ project, repoRoot: root }) as any;
    let registrations = 0;
    for (let line = 1; line <= routes.getEndLineNumber(); line++) {
      if (inf.registrationAtLine(routes, line)) registrations++;
    }
    assert.ok(registrations >= 200, `${registrations} registrations`);
  });

  it('1. every lookup names the node the per-item walk named', () => {
    const inf = new TypeInferrer({ project, repoRoot: root }) as any;
    let asked = 0;
    for (const [label, file] of Object.entries(files)) {
      const sf: SourceFile = project.getSourceFileOrThrow(file);
      const nodes = sf.getDescendants();
      const calls = sf.getDescendantsOfKind(SyntaxKind.CallExpression);
      // The walk costs the file's size per lookup: the large files are sampled.
      const limit = { client: 6, routes: 40, shapes: Infinity }[label] ?? Infinity;
      const fail = (what: string, expected: Node | undefined, actual: Node | undefined) =>
        assert.fail(`${label} ${what}: the walk named ${identity(expected)}, the index ${identity(actual)}`);

      // Registrations: every line, past both ends.
      const last = sf.getEndLineNumber();
      const step = label === 'client' ? Math.ceil(last / 20) : 1;
      for (let line = -1; line <= last + 2; line += step) {
        const expected = referenceRegistrationAtLine(inf, nodes, line);
        const actual = inf.registrationAtLine(sf, line);
        asked++;
        if (expected?.registration !== actual?.registration || expected?.handler !== actual?.handler) {
          fail(`registration at line ${line}`, expected?.registration, actual?.registration);
        }
      }

      // Spans: each node's own (the statement/expression tie included), shifted
      // one byte on both ends (a 1-based SWC span), widened by one, and empty.
      const spanNodes = sample(nodes, limit);
      for (const node of spanNodes) {
        const s = node.getStart();
        const e = node.getEnd();
        for (const [a, b] of [
          [s, e],
          [s + 1, e + 1],
          [s - 1, e + 1],
          [s, s],
          [e, e],
        ]) {
          asked++;
          const expected = referenceFindNodeAtSpan(nodes, a, b);
          const actual = inf.findNodeAtSpan(sf, a, b);
          if (expected !== actual) fail(`node at span ${a}-${b}`, expected, actual);
          const expectedCall = referenceFindCallExpressionAtSpan(calls, a, b);
          const actualCall = inf.findCallExpressionAtSpan(sf, a, b);
          if (expectedCall !== actualCall) fail(`call at span ${a}-${b}`, expectedCall, actualCall);
        }
      }

      // Texts: each node's text at its line, at the window's edges and just
      // past them, and with no line.
      const textNodes = sample(
        nodes.filter((n) => n.getKind() !== SyntaxKind.SyntaxList),
        limit
      );
      for (const node of [...textNodes, ...sample(calls, limit / 2)]) {
        const text = node.getText();
        const line = node.getStartLineNumber();
        for (const at of [line, line - 5, line + 5, line + 6, undefined]) {
          if (limit !== Infinity && at === undefined) continue;
          asked++;
          const expected = referenceFindNodeByText(inf, nodes, text, at);
          const actual = inf.findNodeByText(sf, text, at);
          if (expected !== actual) fail(`node by text ${JSON.stringify(text.slice(0, 40))} @${at}`, expected, actual);
          const expectedCall = referenceFindCallExpressionByText(inf, calls, text, at);
          const actualCall = inf.findCallExpressionByText(sf, text, at);
          if (expectedCall !== actualCall) {
            fail(`call by text ${JSON.stringify(text.slice(0, 40))} @${at}`, expectedCall, actualCall);
          }
        }
      }
    }
    assert.ok(asked > 800, `${asked} lookups asked`);
  });

  it('2. the index walks the nodes ts-morph walks, in its order', () => {
    for (const file of Object.values(files)) {
      const sf = project.getSourceFileOrThrow(file);
      const walked = sf.getDescendants().map((n) => n.compilerNode);
      const indexed = descendantsOf(sf).map((e) => e.node);
      assert.strictEqual(indexed.length, walked.length);
      walked.forEach((n, k) => assert.strictEqual(indexed[k], n, `${file} node ${k}`));
      const walkedCalls = sf.getDescendantsOfKind(SyntaxKind.CallExpression).map((n) => n.compilerNode);
      const indexedCalls = callsOf(sf).map((e) => e.node);
      assert.strictEqual(indexedCalls.length, walkedCalls.length);
      walkedCalls.forEach((n, k) => assert.strictEqual(indexedCalls[k], n, `${file} call ${k}`));
    }
  });

  it('3. one infer batch walks each file once, however many items it holds', () => {
    // A fresh project: no file of it has been indexed yet.
    const fresh = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
    const client = fs.readFileSync(files.client, 'utf8');
    const routes = fs.readFileSync(files.routes, 'utf8');
    const lineOf = (text: string, needle: string) => text.slice(0, text.indexOf(needle)).split('\n').length;
    const requests: InferRequestItem[] = [];
    for (let i = 0; i < 40; i += 4) {
      const getLine = lineOf(client, `.get<GetThing${i}Responses`);
      const bodyAt = client.indexOf(`{ name: 'thing${i}', n: ${i} }`);
      requests.push(
        {
          file_path: files.client,
          line_number: getLine,
          expression_text: `(options.client ?? client).get<GetThing${i}Responses, GetThing${i}Errors, ThrowOnError>({ security: [{ scheme: 'bearer', type: 'http' }], url: '/things/{id}', ...options })`,
          expression_line: getLine,
          infer_kind: 'call_result',
          alias: `Get${i}`,
        },
        {
          file_path: files.client,
          line_number: lineOf(client, `body: { name: 'thing${i}'`),
          span_start: bodyAt,
          span_end: bodyAt + `{ name: 'thing${i}', n: ${i} }`.length,
          infer_kind: 'expression',
          alias: `Body${i}`,
        },
        {
          file_path: files.routes,
          line_number: lineOf(routes, `app.get('/r${i * 5}', handler${i * 5})`),
          infer_kind: 'response_body',
          alias: `Route${i}`,
        }
      );
    }
    const proto = Node.prototype as unknown as {
      _getCompilerDescendantsIterator: (this: Node) => Iterable<unknown>;
    };
    const original = proto._getCompilerDescendantsIterator;
    const walks = new Map<object, number>();
    proto._getCompilerDescendantsIterator = function (this: Node) {
      if (Node.isSourceFile(this)) walks.set(this.compilerNode, (walks.get(this.compilerNode) ?? 0) + 1);
      return original.call(this);
    };
    const builds = nodeIndexBuilds();
    let result;
    try {
      result = new TypeInferrer({ project: fresh, repoRoot: root }).infer(requests);
    } finally {
      proto._getCompilerDescendantsIterator = original;
    }
    assert.ok((result.inferred_types ?? []).length >= requests.length / 2, 'the requests should answer');
    assert.ok(walks.size >= 2, `both files were walked (${walks.size})`);
    for (const count of walks.values()) {
      assert.strictEqual(count, 1, `a file was walked ${count} times for ${requests.length} items`);
    }
    assert.strictEqual(nodeIndexBuilds() - builds, walks.size);
  });
});
