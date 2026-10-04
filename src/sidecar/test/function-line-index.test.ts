/**
 * carrick#1915: the function a line names comes from a per-file index, and
 * the index answers exactly what the walk it replaced answered.
 *
 * `findFunctionByLine` used to walk every node of the file on every call and
 * ask every statement for its start line, which ts-morph computes by counting
 * the file's newlines up to the node. A signature pass asks once per
 * unannotated slot, so a file cost its size times its slots: 81% of the pass
 * on a 2,345-file program.
 *
 * The walk is kept here, and only here, as the reference: `walkReference` is
 * its loop as it stood, fed what the walk recomputed on every call (the
 * functions and statements among the file's descendants, and ts-morph's own
 * start line for each), so its answer is unchanged. Every line of every file
 * below is asked of both, and the two must name the same node.
 *
 * The files: shapes written for the rules (ties, nesting, the forward window,
 * every statement kind, line endings), and every TypeScript file of the
 * sidecar's own source and tests, which is 200-odd files of real code.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  Node,
  Project,
  type ArrowFunction,
  type FunctionDeclaration,
  type FunctionExpression,
  type MethodDeclaration,
  type SourceFile,
} from 'ts-morph';
import { functionAtLine } from '../src/function-line-index.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

type FunctionLike = FunctionDeclaration | ArrowFunction | FunctionExpression | MethodDeclaration;

/**
 * What the walk recomputed on every call, read once per file: the functions
 * and statements among the file's descendants, in the walk's order, and
 * ts-morph's start line for each.
 */
interface WalkInputs {
  functions: FunctionLike[];
  statements: Node[];
  lineOf: (node: Node) => number;
}

function walkInputs(sourceFile: SourceFile): WalkInputs {
  const functions: FunctionLike[] = [];
  const statements: Node[] = [];
  for (const node of sourceFile.getDescendants()) {
    if (
      Node.isFunctionDeclaration(node) ||
      Node.isArrowFunction(node) ||
      Node.isFunctionExpression(node) ||
      Node.isMethodDeclaration(node)
    ) {
      functions.push(node);
    }
    if (Node.isStatement(node)) {
      statements.push(node);
    }
  }
  const lines = new Map<Node, number>();
  return {
    functions,
    statements,
    lineOf: (node) => {
      let line = lines.get(node);
      if (line === undefined) {
        line = node.getStartLineNumber();
        lines.set(node, line);
      }
      return line;
    },
  };
}

/**
 * The walk `findFunctionByLine` was before carrick#1915: the same window, the
 * same rejection, the same loop over the functions in the same order, with
 * `node.getStartLineNumber()` read through `lineOf`.
 */
function walkReference({ functions, statements, lineOf }: WalkInputs, line: number): FunctionLike | undefined {
  const LINE_TOLERANCE = 2;
  /** Statements opening inside the forward window, in source order. */
  const windowStatements = statements.filter((node) => {
    const start = lineOf(node);
    return start >= line && start <= line + LINE_TOLERANCE;
  });

  const separatedFromAnchor = (fn: FunctionLike): boolean =>
    windowStatements.some(
      (statement) =>
        lineOf(statement) < lineOf(fn) &&
        !(statement.getStart() <= fn.getStart() && statement.getEnd() >= fn.getEnd())
    );

  let best: FunctionLike | undefined;
  let bestDelta = Infinity;
  for (const fn of functions) {
    const delta = Math.abs(lineOf(fn) - line);
    if (delta > LINE_TOLERANCE) continue;
    if (lineOf(fn) > line && separatedFromAnchor(fn)) continue;
    const isCloser = delta < bestDelta;
    const isInnermostTie =
      delta === bestDelta &&
      best !== undefined &&
      fn.getEnd() - fn.getStart() < best.getEnd() - best.getStart();
    if (isCloser || isInnermostTie) {
      best = fn;
      bestDelta = delta;
    }
  }
  return best;
}

const describeNode = (node: Node | undefined): string =>
  node ? `${node.getKindName()} [${node.getStart()}, ${node.getEnd()})` : 'no function';

/** How many lines the walk counts in a text: one more than its `\n`s. */
const lineCount = (text: string): number => text.split('\n').length;

/**
 * Ask both of every line of the file, from before its first to past its
 * last. Returns how many lines were asked and how many named a function.
 */
function assertSameAsWalk(sourceFile: SourceFile, label: string): { asked: number; named: number } {
  const inputs = walkInputs(sourceFile);
  const last = lineCount(sourceFile.getFullText());
  let asked = 0;
  let named = 0;
  for (let line = -3; line <= last + 4; line++) {
    const expected = walkReference(inputs, line);
    const actual = functionAtLine(sourceFile, line);
    asked += 1;
    if (expected) named += 1;
    if (actual !== expected) {
      assert.fail(
        `${label}:${line}: the walk names ${describeNode(expected)}, the index ${describeNode(actual)}`
      );
    }
  }
  return { asked, named };
}

const project = (): Project =>
  new Project({ useInMemoryFileSystem: true, compilerOptions: { allowJs: true } });

let fileNumber = 0;
const fileOf = (text: string, extension = 'ts', inProject: Project = project()): SourceFile =>
  inProject.createSourceFile(`/shapes/file${fileNumber++}.${extension}`, text);

/** The 1-based line holding `needle`, which must occur once. */
function lineHolding(text: string, needle: string): number {
  const at = text.indexOf(needle);
  assert.ok(at >= 0 && text.indexOf(needle, at + 1) < 0, `"${needle}" must occur exactly once`);
  return text.slice(0, at).split('\n').length;
}

const textOf = (node: Node | undefined): string | undefined => node?.getText();

describe('the function a line names (carrick#1915)', () => {
  it('takes the function that starts closest to the line, within two lines either way', () => {
    const text = [
      'function far() {}', // 1
      '', // 2
      '', // 3
      '', // 4
      'function near() {}', // 5
      '', // 6
      '', // 7
      '', // 8
      '', // 9
    ].join('\n');
    const file = fileOf(text);
    assert.strictEqual(textOf(functionAtLine(file, 5)), 'function near() {}');
    assert.strictEqual(textOf(functionAtLine(file, 7)), 'function near() {}');
    assert.strictEqual(textOf(functionAtLine(file, 3)), 'function far() {}');
    assert.strictEqual(functionAtLine(file, 8), undefined);
    assertSameAsWalk(file, 'closest');
  });

  it('breaks a tie on distance toward the smaller function', () => {
    const text = [
      'const outer = (a: number) => (b: number) => a + b;', // both start on line 1
      '',
      'const longerName = function () { return 1; };', // 3
      '// anchor', // 4
      'const s = () => 2;', // 5
    ].join('\n');
    const file = fileOf(text);
    assert.strictEqual(textOf(functionAtLine(file, 1)), '(b: number) => a + b');
    // One line from each: the smaller of the two.
    assert.strictEqual(textOf(functionAtLine(file, 4)), '() => 2');
    assertSameAsWalk(file, 'smaller on a tie');
  });

  it('breaks a tie on distance and size toward the function that comes first', () => {
    const text = [
      'const a = () => 1, b = () => 2;', // same line, same size
      '',
      'const p = () => 3;', // 3
      '// anchor', // 4
      'const q = () => 4;', // 5
    ].join('\n');
    const file = fileOf(text);
    assert.strictEqual(textOf(functionAtLine(file, 1)), '() => 1');
    assert.strictEqual(textOf(functionAtLine(file, 4)), '() => 3');
    assertSameAsWalk(file, 'first on a full tie');
  });

  it('reaches forward to a handler that starts inside the statement on the line', () => {
    const text = [
      'declare function register(path: string, handler: () => unknown): void;',
      '',
      '',
      'register(', // 4
      '  "/orders",',
      '  async () => ({ ok: true })', // 6
      ');',
    ].join('\n');
    const file = fileOf(text);
    assert.strictEqual(textOf(functionAtLine(file, 4)), 'async () => ({ ok: true })');
    assertSameAsWalk(file, 'forward reach');
  });

  it('does not reach forward past a statement that does not hold the function', () => {
    const text = [
      'declare const entry: unknown, read: unknown;',
      '',
      '',
      'export { entry, read };', // 4
      '',
      'function helper(raw: string) { return raw; }', // 6
    ].join('\n');
    const file = fileOf(text);
    assert.strictEqual(functionAtLine(file, 4), undefined);
    // From the blank line nothing stands between.
    assert.strictEqual(
      textOf(functionAtLine(file, 5)),
      'function helper(raw: string) { return raw; }'
    );
    assertSameAsWalk(file, 'forward window');
  });

  /**
   * Each statement, alone on a line with a function on the next line and
   * nothing for five lines before it: the line names no function exactly when
   * the statement counts as one.
   */
  it('counts every kind of statement in the forward window', () => {
    const statements: Array<{ lead?: string; statement: string }> = [
      { statement: '{ x; }' },
      { lead: 'while (x)', statement: 'break;' },
      { lead: 'while (x)', statement: 'continue;' },
      { statement: 'class C {}' },
      { statement: 'debugger;' },
      { statement: 'do x; while (x);' },
      { statement: ';' },
      { statement: 'enum E { A }' },
      { statement: 'export default x;' },
      { statement: 'export { x };' },
      { statement: 'x;' },
      { statement: 'for (const k in x) x;' },
      { statement: 'for (const k of x) x;' },
      { statement: 'for (;;) x;' },
      { statement: 'if (x) x;' },
      { statement: 'import "m";' },
      { statement: 'import r = require("m");' },
      { statement: 'interface I {}' },
      { statement: 'label: x;' },
      { lead: 'namespace Split', statement: '{ }' },
      { statement: 'namespace N {}' },
      { statement: 'return x;' },
      { statement: 'switch (x) {}' },
      { statement: 'throw x;' },
      { statement: 'try {} catch {}' },
      { statement: 'type T = string;' },
      { statement: 'const v = 1;' },
      { statement: 'while (x) x;' },
      { statement: 'with (x) x;' },
    ];
    for (const [index, { lead, statement }] of statements.entries()) {
      const text = ['declare const x: any;', '', '', '', '', lead ?? '', statement, `function after${index}() {}`, ''].join(
        '\n'
      );
      const file = fileOf(text);
      assert.strictEqual(
        functionAtLine(file, 7),
        undefined,
        `"${statement}" stands between its line and the function after it`
      );
      assertSameAsWalk(file, `statement "${statement}"`);
    }
  });

  it('lets a comment or a blank line stand between the line and the function', () => {
    for (const trivia of ['// a comment', '/* a comment */', '']) {
      const text = ['declare const x: any;', '', '', '', '', '', trivia, 'function after() {}', ''].join('\n');
      const file = fileOf(text);
      assert.strictEqual(textOf(functionAtLine(file, 7)), 'function after() {}', `after "${trivia}"`);
      assertSameAsWalk(file, `trivia "${trivia}"`);
    }
  });

  it('starts a documented or decorated function where its own text starts', () => {
    const text = [
      'declare function log(): MethodDecorator;',
      'class Orders {',
      '  /**',
      '   * Three lines of comment.',
      '   */',
      '  list() { return []; }', // 6
      '',
      '',
      '',
      '  @log()', // 10: the method starts at its decorator
      '  create() { return 1; }',
      '  get total() { return 0; }',
      '  constructor() {}',
      '}',
    ].join('\n');
    const file = fileOf(text);
    // The comment's first line is not where `list` starts: it is three lines
    // from the method and two from the declaration above the class.
    assert.strictEqual(textOf(functionAtLine(file, 3)), 'declare function log(): MethodDecorator;');
    assert.strictEqual(textOf(functionAtLine(file, 6)), 'list() { return []; }');
    assert.strictEqual(lineHolding(text, '@log()'), 10);
    assert.match(textOf(functionAtLine(file, 10)) ?? '', /^@log\(\)\s+create\(\)/);
    assertSameAsWalk(file, 'documented and decorated');
  });

  it('counts lines as the walk did: by line feeds alone', () => {
    const crlf = ['function one() {}', '', 'const two = () => 2;', '', '', '', 'function three() {}'].join('\r\n');
    // A lone carriage return and a line separator each end a line for the
    // compiler, and neither moved the walk's line numbers.
    const loneCarriageReturn = 'const a = 1;\r\rfunction one() {}\n\n\n\nconst two = () => 2;\n';
    const lineSeparator = 'const a = `x y`;  function one() {}\n\n\n\nconst two = () => 2;\n';
    const shebang = '#!/usr/bin/env node\nfunction one() {}\n\n\n\nconst two = () => 2;\n';
    for (const [label, text] of Object.entries({ crlf, loneCarriageReturn, lineSeparator, shebang })) {
      const { named } = assertSameAsWalk(fileOf(text), label);
      assert.ok(named > 0, `${label}: some line names a function`);
    }
    assert.strictEqual(textOf(functionAtLine(fileOf(crlf), 3)), '() => 2');
    assert.strictEqual(textOf(functionAtLine(fileOf(loneCarriageReturn), 1)), 'function one() {}');
  });

  it('agrees with the walk across function shapes', () => {
    const ts = `
export default function main() {}
export function* generate() { yield 1; }
function overload(a: string): string;
function overload(a: number): number;
function overload(a: unknown): unknown { return a; }
declare function ambient(): void;
const handlers = {
  method() { return 1; },
  arrow: () => 2,
  fn: function named() { return 3; },
  async *stream() {},
  nested: { deep() { return () => () => 4; } },
};
(function iife() {
  function inner() {
    return function () { return 5; };
  }
  return inner;
})();
const withDefault = (callback = () => 6, other = function () {}) => callback;
const inTemplate = \`\${(() => 7)()} and \${function () { return 8; }}\`;
namespace Space {
  export function inSpace() {}
  export namespace Inner { export const f = () => 9; }
}
switch (1 as number) {
  case 1: {
    const inCase = () => 10;
    break;
  }
  default:
    (() => 11)();
}
label: for (const item of [1, 2]) {
  [item].map((value) => value).filter(function (value) { return value > 1; });
}
abstract class Shape {
  static create = () => new (class extends Shape { area() { return 0; } })();
  abstract area(): number;
  private helper = function () { return 12; };
  method<T>(
    value: T
  ): T {
    return value;
  }
}
const chained = [1].map((a) => a)
  .map((b) => b)
  .map((c) => c);
const a1 = () => 1; const a2 = () => 2; const a3 = function () {};
`;
    assertSameAsWalk(fileOf(ts), 'function shapes');

    const tsx = `
export const List = ({ items }: { items: string[] }) => (
  <ul onClick={() => undefined}>
    {items.map((item) => (
      <li key={item} onMouseOver={function () {}}>{item}</li>
    ))}
  </ul>
);
function Page() {
  return <List items={[]} />;
}
`;
    assertSameAsWalk(fileOf(tsx, 'tsx'), 'jsx');

    const js = `
module.exports = function (req, res) { res.send(1); };
exports.handler = async (event) => ({ statusCode: 200 });
class Legacy { run() {} }
`;
    assertSameAsWalk(fileOf(js, 'js'), 'javascript');
    assertSameAsWalk(fileOf(''), 'empty file');
    assertSameAsWalk(fileOf('\n\n\n'), 'blank file');
  });

  it('answers nothing for a line that is not a number of lines', () => {
    const file = fileOf('function one() {}\n');
    for (const line of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      assert.strictEqual(functionAtLine(file, line), undefined);
      assert.strictEqual(walkReference(walkInputs(file), line), undefined);
    }
  });

  it('reads the file again after its text is replaced, and again when it is restored', () => {
    const original = ['function one() {}', '', '', '', '', 'function two() {}', ''].join('\n');
    const file = fileOf(original);
    assert.strictEqual(textOf(functionAtLine(file, 6)), 'function two() {}');

    file.replaceWithText(['', '', '', 'function moved() {}', '', '', '', '', '', 'function late() {}'].join('\n'));
    assert.strictEqual(textOf(functionAtLine(file, 4)), 'function moved() {}');
    assert.strictEqual(textOf(functionAtLine(file, 10)), 'function late() {}');
    assert.strictEqual(functionAtLine(file, 1), undefined);
    assertSameAsWalk(file, 'replaced text');

    file.replaceWithText(original);
    assert.strictEqual(textOf(functionAtLine(file, 6)), 'function two() {}');
    assert.strictEqual(textOf(functionAtLine(file, 1)), 'function one() {}');
    assertSameAsWalk(file, 'restored text');
  });

  it('agrees with the walk on every line of the sidecar\'s own source and tests', () => {
    // dist/test -> the sidecar package root.
    const root = path.resolve(__dirname, '..', '..');
    const files: string[] = [];
    const collect = (dir: string) => {
      for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        if (entry.name === 'node_modules') continue;
        const full = path.join(dir, entry.name);
        if (entry.isDirectory()) collect(full);
        else if (/\.(ts|tsx|mts|cts)$/.test(entry.name)) files.push(full);
      }
    };
    collect(path.join(root, 'src'));
    collect(path.join(root, 'test'));
    files.sort();
    assert.ok(files.length > 150, `expected the sidecar's own files, found ${files.length}`);

    let asked = 0;
    let named = 0;
    for (const file of files) {
      // A project per file: the walk's wrappers for every token go with it.
      const sourceFile = fileOf(fs.readFileSync(file, 'utf8'), path.extname(file).slice(1));
      const counts = assertSameAsWalk(sourceFile, path.relative(root, file));
      asked += counts.asked;
      named += counts.named;
    }
    assert.ok(asked > 50_000, `expected tens of thousands of lines, asked ${asked}`);
    assert.ok(named > 10_000, `expected thousands of lines to name a function, got ${named}`);
  });
});
