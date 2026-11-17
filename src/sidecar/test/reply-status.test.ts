/**
 * the one reader of a reply's HTTP status, run on a raw
 * `ts.createProgram` (no ts-morph, no labels). One case per form the
 * inferrer's reading handles; the module is the same one the inferrer calls.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import ts from 'typescript';
import { isDroppedReplyStatus, replyStatus, valueStatus } from '../src/capture/reply-status.js';

const DTS = `
export declare enum Code { Gone = 410, Fine = 200 }
export interface Chain { json(body: unknown): void; }
export declare const rep: {
  send(body: unknown, status?: number | { status: number }): void;
  status(code: number): Chain;
  redirect(url: string): { status: 302 };
  notFound(body: unknown): { status: 404 };
  typed(body: unknown): { status: 200 | 404 };
  ok(body: unknown): { status: 200 };
};
`;

const SRC = `
import { rep, Code } from 'tiny-reply';
declare const n: number;
declare const either: 200 | 404;
const gone = 410 as const;
export function run() {
  rep.send({ a: 1 }, 404);
  rep.send({ a: 1 }, { status: 401 });
  rep.send({ a: 1 }, gone);
  rep.send({ a: 1 }, n);
  rep.send({ a: 1 }, either);
  rep.send({ a: 1 }, Code.Gone);
  rep.send(404);
  rep.status(401).json({ a: 1 });
  rep.status(n).json({ a: 1 });
  rep.redirect('/x');
  rep.notFound({ a: 1 });
  rep.typed({ a: 1 });
  rep.ok({ a: 1 });
  rep.send({ a: 1 });
  rep.send({ a: 1 }, 201);
}
`;

function program() {
  const files = new Map<string, string>([
    ['/p/main.ts', SRC],
    ['/p/node_modules/tiny-reply/index.d.ts', DTS],
    ['/p/node_modules/tiny-reply/package.json', '{"name":"tiny-reply","types":"index.d.ts"}'],
  ]);
  const options: ts.CompilerOptions = {
    strict: true,
    noEmit: true,
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    types: [],
    lib: ['lib.es2022.d.ts'],
  };
  const host = ts.createCompilerHost(options);
  const read = host.readFile.bind(host);
  const exists = host.fileExists.bind(host);
  const getSource = host.getSourceFile.bind(host);
  host.fileExists = (f) => files.has(f) || exists(f);
  const dirExists = host.directoryExists?.bind(host);
  host.directoryExists = (d) =>
    [...files.keys()].some((f) => f.startsWith(`${d}/`)) || (dirExists?.(d) ?? false);
  host.readFile = (f) => files.get(f) ?? read(f);
  host.getSourceFile = (f, lang, onError, shouldCreate) =>
    files.has(f) ? ts.createSourceFile(f, files.get(f)!, lang, true) : getSource(f, lang, onError, shouldCreate);
  const prog = ts.createProgram(['/p/main.ts'], options, host);
  const checker = prog.getTypeChecker();
  const calls: ts.CallExpression[] = [];
  const visit = (node: ts.Node): void => {
    if (ts.isCallExpression(node)) calls.push(node);
    ts.forEachChild(node, visit);
  };
  visit(prog.getSourceFile('/p/main.ts')!);
  return { checker, calls };
}

/** The outermost call written on each line of `run`, in order. */
function statements(calls: ts.CallExpression[]): ts.CallExpression[] {
  return calls.filter((c) => ts.isExpressionStatement(c.parent));
}

describe('reply-status: one reader of a reply status', () => {
  const { checker, calls } = program();
  const stmts = statements(calls);
  const at = (i: number) => stmts[i];
  const plain = (i: number) => replyStatus(checker, at(i), { readChain: false });
  const chained = (i: number) => replyStatus(checker, at(i), { readChain: true });

  it('reads a literal argument past the body', () => assert.strictEqual(plain(0), 'error'));
  it('reads an options object', () => assert.strictEqual(plain(1), 'error'));
  it('reads a literal-typed variable', () => assert.strictEqual(plain(2), 'error'));
  it('reads a plain number as variable', () => assert.strictEqual(plain(3), 'variable'));
  it('reads 200 | 404 as variable', () => assert.strictEqual(plain(4), 'variable'));
  it('reads a numeric enum member', () => assert.strictEqual(plain(5), 'error'));
  it('ignores a status in the first argument', () => assert.strictEqual(plain(6), 'undecided'));
  it('reads the chain only when asked', () => {
    assert.strictEqual(plain(7), 'undecided');
    assert.strictEqual(chained(7), 'error');
    assert.strictEqual(chained(8), 'variable');
  });
  it('reads a status the result type records', () => {
    assert.strictEqual(plain(10), 'error');
    assert.strictEqual(plain(12), 'success');
  });
  it('reads a redirect result default 302', () => assert.strictEqual(plain(9), 'redirect'));
  it('reads a typed default spanning success and error as undecided', () =>
    assert.strictEqual(plain(11), 'undecided'));
  it('reads no status as undecided and 201 as success', () => {
    assert.strictEqual(plain(13), 'undecided');
    assert.strictEqual(plain(14), 'success');
  });
  it('drops only error and redirect', () => {
    assert.strictEqual(isDroppedReplyStatus('error'), true);
    assert.strictEqual(isDroppedReplyStatus('redirect'), true);
    for (const s of ['success', 'variable', 'undecided'] as const) {
      assert.strictEqual(isDroppedReplyStatus(s), false);
    }
  });
  it('valueStatus of nothing is undecided', () => assert.strictEqual(valueStatus(checker, undefined), 'undecided'));
});
