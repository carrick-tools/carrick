/**
 * carrick#2019: a type from a module the enclosing file does not import
 * prints as `import("<declaring file>").Name`, whichever file it is printed
 * from, as it did on TypeScript 5.
 *
 * TypeScript 6 gives every print a module-specifier host, so the compiler's
 * own `typeToString` at an enclosing node writes `import("../deep/model")` or
 * `import("lib")`: text that depends on the file it was printed from, which a
 * capture pastes into another file and the scanner's path scrub cannot read.
 * `typeToStringAsDeclared` is `typeToString` without that host; on every type
 * whose print names no module it must give `typeToString`'s exact text.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Project, ts } from 'ts-morph';
import { typeToStringAsDeclared, typeToTypeNodeAsDeclared } from '../src/print-type.js';
import { SidecarClient } from './helpers.js';

const tempRoots: string[] = [];
after(() => {
  for (const root of tempRoots) fs.rmSync(root, { recursive: true, force: true });
});

function writeRepo(files: Record<string, string>): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2019-print-')));
  tempRoots.push(root);
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return root;
}

const FILES = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022' },
    include: ['src'],
  }),
  'src/deep/model.ts': 'export interface Model { id: string }\nexport function make(): Model { return { id: "x" }; }\n',
  'src/a/use.ts': [
    'import { make } from "../deep/model";',
    'export function get() { return make(); }',
    'export function pair() { return [make(), 1 as const] as const; }',
    'export async function later() { return [make()]; }',
    '',
  ].join('\n'),
  'src/b/c/other.ts': [
    'import { make } from "../../deep/model";',
    'export function again() { return make(); }',
    'export async function laterAgain() { return [make()]; }',
    '',
  ].join('\n'),
  'src/b/lib-use.ts': 'import { box } from "lib";\nexport function get() { return box(); }\n',
  'node_modules/lib/package.json': JSON.stringify({ name: 'lib', version: '1.2.3', types: 'dist/index.d.ts' }),
  'node_modules/lib/dist/index.d.ts': 'export { Box } from "./box";\nexport declare function box(): import("./box").Box;\n',
  'node_modules/lib/dist/box.d.ts': 'export interface Box { v: number }\n',
  // Shapes whose print names no module: the clone must match typeToString.
  'src/shapes.ts': [
    'import type { Model } from "./deep/model";',
    "export type Kind = 'a' | 'b' | 'c';",
    'export enum Color { Red, Green }',
    'declare const sym: unique symbol;',
    'export function shapes(m: Model, k: Kind) {',
    '  return {',
    '    m, k, color: Color.Red, sym,',
    '    list: [1, 2, 3],',
    '    tuple: [1, "x"] as [number, string],',
    '    fn: (x: number, y?: string): x is 1 => true,',
    '    map: new Map<string, Model[]>(),',
    '    maybe: Math.random() ? m : undefined,',
    '    mapped: {} as { readonly [P in Kind]?: Promise<P> },',
    '    long: {} as { [K in `field_${number}` | `name_${Kind}_${Kind}_suffix_long_enough`]: Record<K, { deeply: { nested: K[] } }> },',
    '  };',
    '}',
    '',
  ].join('\n'),
};

function program(root: string): { project: Project; checker: ts.TypeChecker } {
  const project = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
  return { project, checker: project.getTypeChecker().compilerObject };
}

function returnOf(project: Project, root: string, file: string, name: string) {
  const fn = project.getSourceFileOrThrow(path.join(root, file)).getFunctionOrThrow(name);
  return { node: fn.compilerNode as ts.Node, type: fn.getReturnType().compilerType };
}

describe('carrick#2019: a printed module is its declaring file, from any enclosing file', () => {
  const root = writeRepo(FILES);
  const { project, checker } = program(root);
  const flags = ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.InTypeAlias;

  it('writes a repo module and a package module as the files that declare them', () => {
    const local = returnOf(project, root, 'src/a/use.ts', 'get');
    const lib = returnOf(project, root, 'src/b/lib-use.ts', 'get');
    assert.strictEqual(
      typeToStringAsDeclared(checker, local.type, local.node, flags),
      `import("${root}/src/deep/model").Model`
    );
    assert.strictEqual(
      typeToStringAsDeclared(checker, lib.type, lib.node, flags),
      `import("${root}/node_modules/lib/dist/box").Box`
    );
  });

  it('prints one type the same from two files at two depths', () => {
    const a = returnOf(project, root, 'src/a/use.ts', 'get');
    const b = returnOf(project, root, 'src/b/c/other.ts', 'again');
    assert.strictEqual(
      typeToStringAsDeclared(checker, a.type, a.node, flags),
      typeToStringAsDeclared(checker, b.type, b.node, flags)
    );
  });

  it('builds the node the same way, keeping a caller tracker of its own', () => {
    const { type, node } = returnOf(project, root, 'src/a/use.ts', 'pair');
    let tracked = 0;
    const built = typeToTypeNodeAsDeclared(checker, type, node, ts.NodeBuilderFlags.NoTruncation | ts.NodeBuilderFlags.IgnoreErrors, {
      trackSymbol: () => {
        tracked++;
        return false;
      },
    } as never);
    const text = ts.createPrinter({ removeComments: true }).printNode(ts.EmitHint.Unspecified, built!, node.getSourceFile());
    assert.match(text, new RegExp(`import\\("${root.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}/src/deep/model"\\)\\.Model`));
    assert.ok(tracked > 0, 'the caller tracker still hears every symbol');
  });

  it("gives typeToString's own text for every print that names no module", () => {
    const shapes = project.getSourceFileOrThrow(path.join(root, 'src/shapes.ts'));
    const fn = shapes.getFunctionOrThrow('shapes');
    const types = [fn.getReturnType(), ...fn.getReturnType().getProperties().map((p) => p.getTypeAtLocation(fn))];
    let compared = 0;
    for (const type of types) {
      for (const f of [undefined, flags, ts.TypeFormatFlags.InTypeAlias, ts.TypeFormatFlags.UseFullyQualifiedType]) {
        for (const enclosing of [fn.compilerNode, undefined]) {
          const own = checker.typeToString(type.compilerType, enclosing, f);
          if (own.includes('import(')) continue;
          assert.strictEqual(typeToStringAsDeclared(checker, type.compilerType, enclosing, f), own, own);
          compared++;
        }
      }
    }
    assert.ok(compared >= 60, `compared ${compared} prints`);
    // The default flags truncate a long print, as typeToString does.
    const whole = fn.getReturnType().compilerType;
    const cut = typeToStringAsDeclared(checker, whole, fn.compilerNode);
    assert.ok(cut.endsWith('...') && cut === checker.typeToString(whole, fn.compilerNode), cut);
  });
});

describe('carrick#2019: an inference names a module by its declaring file, through the process', () => {
  it('infers the return of a call into a module the file does not import as that module\'s file', async () => {
    const root = writeRepo(FILES);
    const client = new SidecarClient();
    await client.start();
    try {
      const init = await client.send<{ status: string }>({ action: 'init', request_id: 'init', repo_root: root });
      assert.strictEqual(init.status, 'ready');
      const res = await client.send<{ inferred_types?: Array<{ alias: string; type_string: string }> }>(
        {
          action: 'infer',
          request_id: 'infer',
          requests: [
            { file_path: path.join(root, 'src/a/use.ts'), line_number: 4, infer_kind: 'signature_return', alias: 'Later' },
            { file_path: path.join(root, 'src/b/c/other.ts'), line_number: 3, infer_kind: 'signature_return', alias: 'LaterAgain' },
          ],
        },
        60000
      );
      const rows = new Map((res.inferred_types ?? []).map((r) => [r.alias, r.type_string]));
      assert.strictEqual(rows.get('Later'), `Promise<import("${root}/src/deep/model").Model[]>`);
      assert.strictEqual(rows.get('LaterAgain'), rows.get('Later'));
    } finally {
      await client.stop();
    }
  });
});

describe('carrick#2019: no print at an enclosing node leaves the module host to the compiler', () => {
  // dist/test/ -> the sidecar root -> src/.
  const SRC = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'src');
  // The two modules that build type nodes with a host decided on purpose: this
  // one (none) and the capture's surface print (the entry file's own).
  const OWN_HOST = new Set(['print-type.ts', path.join('capture', 'node-builder.ts')]);

  function* sources(dir: string): Generator<string> {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const abs = path.join(dir, entry.name);
      if (entry.isDirectory()) yield* sources(abs);
      else if (entry.name.endsWith('.ts')) yield abs;
    }
  }

  const isUndefined = (node: ts.Expression | undefined): boolean =>
    node === undefined || (ts.isIdentifier(node) && node.text === 'undefined');

  it('finds every typeToString, typeToTypeNode and Type.getText at an enclosing node going through print-type', () => {
    const offenders: string[] = [];
    for (const abs of sources(SRC)) {
      const rel = path.relative(SRC, abs);
      if (OWN_HOST.has(rel)) continue;
      const file = ts.createSourceFile(abs, fs.readFileSync(abs, 'utf8'), ts.ScriptTarget.Latest, true);
      const visit = (node: ts.Node): void => {
        if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) {
          const name = node.expression.name.text;
          const [first, second] = node.arguments;
          const receiver = node.expression.expression.getText(file);
          // The checker's printers take the enclosing node second.
          const checkerPrint = (name === 'typeToString' || name === 'typeToTypeNode') && !isUndefined(second);
          // ts-morph's Type.getText(enclosingNode, flags): a type receiver, or flags beside the node.
          const typePrint =
            name === 'getText' && !isUndefined(first) && (second !== undefined || /[tT]ype(\(\))?$/.test(receiver));
          if (checkerPrint || typePrint) {
            offenders.push(`${rel}:${file.getLineAndCharacterOfPosition(node.getStart(file)).line + 1}`);
          }
        }
        ts.forEachChild(node, visit);
      };
      visit(file);
    }
    assert.deepStrictEqual(offenders, []);
  });
});
