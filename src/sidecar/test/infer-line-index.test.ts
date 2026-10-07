/**
 * carrick#1935: the text locator asks a file's lines from an index, and the
 * index names exactly the node the per-node lookup named.
 *
 * `findNodeByText` walked every node of a file and asked each for its start
 * line, which ts-morph computes by counting the file's line feeds from
 * position 0. On a generated client of several thousand lines that is the
 * file's size once per node, per request.
 *
 * - Test 1 counts `getStartLineNumber` calls (no clock) for text-located
 *   requests on a generated SDK. It fails on the walk.
 * - Test 2 keeps the walk as it stood (`referenceMatch`: `matchByText` with
 *   ts-morph's own line) and asks both of every call text and a sample of
 *   node texts, in generated and real files, line endings included.
 * - Test 3 pins `startLineOf` / `endLineOf` to ts-morph for every node, and
 *   shows the index is rebuilt after the file is rewritten.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Node, Project, SyntaxKind, type SourceFile } from 'ts-morph';
import { endLineOf, startLineOf } from '../src/line-index.js';
import { TypeInferrer } from '../src/type-inferrer.js';
import type { InferRequestItem } from '../src/types.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const FUNCTIONS = 150;

/** A generated SDK the test writes itself: result-shaped client, N functions. */
function sdkSource(n: number): string {
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
      `export type GetThing${i}Data = { id: number };`,
      `export type GetThing${i}Responses = { 200: { name: string; n${i}: number } };`,
      `export type GetThing${i}Errors = { 404: { message: string } };`,
      `export const getThing${i} = <ThrowOnError extends boolean = false>(options: Options<GetThing${i}Data, ThrowOnError>) =>`,
      `    (options.client ?? client).get<GetThing${i}Responses, GetThing${i}Errors, ThrowOnError>({`,
      `        security: [{ scheme: 'bearer', type: 'http' }],`,
      `        url: '/things/{id}',`,
      `        ...options`,
      `    });`,
      `export const postThing${i} = <ThrowOnError extends boolean = false>(options: Options<{ name: string }, ThrowOnError>) =>`,
      `    (options.client ?? client).post<GetThing${i}Responses, GetThing${i}Errors, ThrowOnError>({`,
      `        url: '/things',`,
      `        body: { name: 'thing${i}', n: ${i} },`,
      `        ...options`,
      `    });`
    );
  }
  return lines.join('\n') + '\n';
}

function lineOfText(source: string, needle: string): number {
  const idx = source.indexOf(needle);
  assert.ok(idx >= 0, `source must contain ${needle}`);
  return source.slice(0, idx).split('\n').length;
}

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
  include: ['src/**/*.ts'],
});

/**
 * The walk as it stood: `matchByText` with ts-morph's own start line, and
 * `findNodeByText`'s node filter. Everything else in the locator is the
 * inferrer's own, called through.
 */
function referenceMatch(
  inferrer: any,
  sourceFile: SourceFile,
  expressionText: string,
  lineNumber?: number,
  searchRadius = 5
): Node | undefined {
  const nodes = sourceFile
    .getDescendants()
    .filter((n) => !Node.isSourceFile(n) && n.getKind() !== SyntaxKind.SyntaxList);
  const target = inferrer.normalizeWhitespace(expressionText);
  if (!target) return undefined;
  const inWindow = lineNumber
    ? nodes.filter((n) => {
        const l = n.getStartLineNumber();
        return l >= lineNumber - searchRadius && l <= lineNumber + searchRadius;
      })
    : nodes;
  const candidates = inWindow.map((node) => ({ node, text: inferrer.normalizeWhitespace(node.getText()) }));
  if (candidates.length === 0) return undefined;
  const pick = (ns: Node[]): Node =>
    ns.reduce((best, current) => {
      const bestRange = best.getEnd() - best.getStart();
      const currentRange = current.getEnd() - current.getStart();
      if (currentRange !== bestRange) return currentRange < bestRange ? current : best;
      if (lineNumber !== undefined) {
        const bestDist = Math.abs(best.getStartLineNumber() - lineNumber);
        const currentDist = Math.abs(current.getStartLineNumber() - lineNumber);
        return currentDist < bestDist ? current : best;
      }
      return best;
    });
  const exact = candidates.filter((c) => c.text === target);
  if (exact.length > 0) return pick(exact.map((c) => c.node));
  if (/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(target)) return undefined;
  const sub = candidates.filter(
    (c) =>
      c.text.includes(target) ||
      (c.text.length >= 8 && c.text.length >= target.length * 0.5 && target.includes(c.text))
  );
  if (sub.length === 0) return undefined;
  const containing = sub.filter((c) => c.text.includes(target));
  return pick((containing.length > 0 ? containing : sub).map((c) => c.node));
}

const identity = (n: Node | undefined) =>
  n ? `${n.getKindName()}@${n.getStart()}-${n.getEnd()}` : 'none';

describe('text locator lines come from an index (carrick#1935)', () => {
  let root: string;
  let sdk: string;
  let project: Project;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'line-index-'));
    fs.mkdirSync(path.join(root, 'src'));
    fs.writeFileSync(path.join(root, 'tsconfig.json'), TSCONFIG);
    sdk = sdkSource(FUNCTIONS);
    fs.writeFileSync(path.join(root, 'src/sdk.ts'), sdk);
    // The differential walks the reference per text, so it takes a small SDK.
    fs.writeFileSync(path.join(root, 'src/small.ts'), sdkSource(8));
    // Line endings the compiler and ts-morph disagree on.
    fs.writeFileSync(
      path.join(root, 'src/endings.ts'),
      'export const a = { x: 1 };\r\nexport const b = { y: 2 };\rexport const c = { z: 3 }; export const d = { w: 4 };\nexport const e = { v: 5 };\n'
    );
    project = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const inferrer = () => new TypeInferrer({ project, repoRoot: root });

  it('1. a text-located request makes a handful of getStartLineNumber calls, not one per node', () => {
    const file = path.join(root, 'src/sdk.ts');
    const i = 120;
    const callLine = lineOfText(sdk, `.get<GetThing${i}Responses`);
    const postLine = lineOfText(sdk, `body: { name: 'thing${i}', n: ${i} }`);
    const requests: InferRequestItem[] = [
      {
        file_path: file,
        line_number: callLine,
        expression_text: `(options.client ?? client).get<GetThing${i}Responses, GetThing${i}Errors, ThrowOnError>({ security: [{ scheme: 'bearer', type: 'http' }], url: '/things/{id}', ...options })`,
        expression_line: callLine,
        infer_kind: 'call_result',
        alias: 'WithSecurity',
      },
      {
        // Drifted text: the call-only match misses and the node walk answers.
        file_path: file,
        line_number: callLine,
        expression_text: `client.get<GetThing${i}Responses, GetThing${i}Errors, ThrowOnError>({ url: '/things/{id}' })`,
        expression_line: callLine,
        infer_kind: 'call_result',
        alias: 'Drifted',
      },
      {
        file_path: file,
        line_number: postLine,
        expression_text: `{ name: 'thing${i}', n: ${i} }`,
        expression_line: postLine,
        infer_kind: 'request_body',
        alias: 'Payload',
      },
    ];
    const proto = Node.prototype as unknown as { getStartLineNumber: (...a: unknown[]) => number };
    const original = proto.getStartLineNumber;
    let calls = 0;
    proto.getStartLineNumber = function (this: Node, ...args: unknown[]) {
      calls++;
      return original.apply(this, args);
    };
    let result;
    try {
      result = inferrer().infer(requests);
    } finally {
      proto.getStartLineNumber = original;
    }
    assert.ok((result.inferred_types ?? []).length >= 1, 'the requests should answer');
    const nodes = project.getSourceFileOrThrow(file).getDescendants().length;
    assert.ok(nodes > 5000, `the fixture should be large, has ${nodes} nodes`);
    assert.ok(
      calls <= 20 * requests.length,
      `${calls} getStartLineNumber calls for ${requests.length} requests on ${nodes} nodes`
    );
  });

  it('2. the index names the node the per-node lookup named', () => {
    const inf = inferrer() as any;
    const texts = new Map<string, string[]>();
    const sdkFile = project.getSourceFileOrThrow(path.join(root, 'src/small.ts'));
    const sample: string[] = [];
    for (const call of sdkFile.getDescendantsOfKind(SyntaxKind.CallExpression)) sample.push(call.getText());
    const all = sdkFile.getDescendants();
    for (let k = 0; k < all.length; k += 41) sample.push(all[k].getText());
    texts.set(sdkFile.getFilePath(), sample);

    const endings = project.getSourceFileOrThrow(path.join(root, 'src/endings.ts'));
    texts.set(endings.getFilePath(), endings.getDescendants().map((n) => n.getText()));

    const srcDir = path.resolve(__dirname, '../../src');
    const real = [
      ...fs.readdirSync(srcDir).filter((f) => f.endsWith('.ts')).map((f) => path.join(srcDir, f)),
    ].slice(0, 4);
    for (const f of real) {
      const sf = project.addSourceFileAtPath(f);
      const ds = sf.getDescendants();
      const s: string[] = [];
      for (let k = 0; k < ds.length; k += Math.max(1, Math.floor(ds.length / 15))) s.push(ds[k].getText());
      texts.set(sf.getFilePath(), s);
    }

    let compared = 0;
    for (const [file, list] of texts) {
      const sf = project.getSourceFileOrThrow(file);
      for (const text of list) {
        if (text.length > 400) continue;
        for (const line of [undefined, Math.max(1, Math.floor(sf.getEndLineNumber() / 2))]) {
          const expected = identity(referenceMatch(inf, sf, text, line));
          const actual = identity(inf.findNodeByText(sf, text, line));
          assert.strictEqual(actual, expected, `${path.basename(file)} line=${line} text=${text.slice(0, 60)}`);
          compared++;
        }
      }
    }
    assert.ok(compared > 100, `compared ${compared}`);
  });

  it('3. startLineOf / endLineOf equal ts-morph for every node, and follow a rewrite', () => {
    for (const name of ['small.ts', 'endings.ts']) {
      const sf = project.getSourceFileOrThrow(path.join(root, 'src', name));
      for (const node of [sf, ...sf.getDescendants()]) {
        assert.strictEqual(startLineOf(node), node.getStartLineNumber());
        assert.strictEqual(endLineOf(node), node.getEndLineNumber());
      }
    }
    const sf = project.createSourceFile(path.join(root, 'src/rewrite.ts'), 'export const a = 1;\nexport const b = 2;\n');
    const second = () => sf.getVariableStatementOrThrow('b');
    assert.strictEqual(startLineOf(second()), 2);
    sf.replaceWithText('// one\n// two\n// three\nexport const a = 1;\nexport const b = 2;\n');
    assert.strictEqual(startLineOf(second()), 5);
    assert.strictEqual(startLineOf(second()), second().getStartLineNumber());
    assert.strictEqual(endLineOf(second()), second().getEndLineNumber());
  });
});
