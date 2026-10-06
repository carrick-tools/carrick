/**
 * carrick#2019: the sidecar runs on TypeScript 6, and every program it builds
 * carries `stableTypeOrdering` and TypeScript 5's value for each option whose
 * default 6 changed and the project leaves unset.
 *
 * - A type prints the same text in a process whatever that process checked
 *   first: a union's members, and the properties of an object a mapped type
 *   made, no longer come out in the order the checker met them.
 * - A project's tsconfig that leaves `strict`, `types`, `target`, `module`,
 *   `moduleResolution` or the interop options unset is read as TypeScript 5
 *   reads it: no strict null checks, every `@types/*` package, ES5, CommonJS,
 *   node10 resolution, no synthetic default imports.
 * - A tsconfig that sets an option TypeScript 6 deprecates (baseUrl,
 *   moduleResolution node, target es5) keeps loading, and so do the
 *   TypeScript 5 values the sidecar fills in: none raises an error.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
import { sidecarCompilerOptions } from '../src/capture/compiler-options.js';
import { CHECKER_TSCONFIG } from '../src/capture/check-workspace.js';
import type { CaptureStubResult } from '../src/capture/api.js';
import { SidecarClient } from './helpers.js';

const tempRoots: string[] = [];
after(() => {
  for (const root of tempRoots) fs.rmSync(root, { recursive: true, force: true });
});

function writeRepo(files: Record<string, string>): string {
  // realpath: on macOS the temp dir is a symlink.
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2019-')));
  tempRoots.push(root);
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return root;
}

/** The program a tsconfig's options give once the sidecar has filled them. */
function programFor(config: unknown): { program: ts.Program; options: ts.CompilerOptions } {
  const root = writeRepo({
    'tsconfig.json': JSON.stringify(config),
    'src/index.ts': 'export const one = 1;\n',
  });
  const parsed = ts.getParsedCommandLineOfConfigFile(path.join(root, 'tsconfig.json'), {}, {
    ...ts.sys,
    onUnRecoverableConfigFileDiagnostic: (d) => assert.fail(ts.flattenDiagnosticMessageText(d.messageText, ' ')),
  })!;
  const options = sidecarCompilerOptions(parsed.options);
  return { program: ts.createProgram({ rootNames: parsed.fileNames, options, configFileParsingDiagnostics: parsed.errors }), options };
}

/** Every option-level error a program reports: its config, its options and its globals. */
function optionErrors(program: ts.Program): string[] {
  return [
    ...program.getConfigFileParsingDiagnostics(),
    ...program.getOptionsDiagnostics(),
    ...program.getGlobalDiagnostics(),
  ]
    .filter((d) => d.category === ts.DiagnosticCategory.Error)
    .map((d) => `TS${d.code}: ${ts.flattenDiagnosticMessageText(d.messageText, ' ')}`);
}

/** The effective value of each option TypeScript 6 changed the default of. */
function effective(options: ts.CompilerOptions): Record<string, unknown> {
  const computed = (ts as unknown as {
    computedOptions: Record<string, { computeValue(o: ts.CompilerOptions): unknown }>;
  }).computedOptions;
  const row: Record<string, unknown> = {};
  for (const key of [
    'target',
    'module',
    'moduleResolution',
    'esModuleInterop',
    'allowSyntheticDefaultImports',
    'resolveJsonModule',
    'resolvePackageJsonExports',
    'useDefineForClassFields',
    'strictNullChecks',
    'noImplicitAny',
    'strictFunctionTypes',
    'useUnknownInCatchVariables',
    'alwaysStrict',
  ]) {
    row[key] = computed[key].computeValue(options);
  }
  row.types = options.types;
  row.noUncheckedSideEffectImports = options.noUncheckedSideEffectImports;
  row.libReplacement = options.libReplacement;
  row.stableTypeOrdering = options.stableTypeOrdering;
  return row;
}

const { ModuleKind: M, ModuleResolutionKind: R, ScriptTarget: T } = ts;

/** TypeScript 5.9's effective values for a config that sets none of these. */
const TS5_EMPTY = {
  target: T.ES5,
  module: M.CommonJS,
  moduleResolution: R.Node10,
  esModuleInterop: false,
  allowSyntheticDefaultImports: false,
  resolveJsonModule: false,
  resolvePackageJsonExports: false,
  useDefineForClassFields: false,
  strictNullChecks: false,
  noImplicitAny: false,
  strictFunctionTypes: false,
  useUnknownInCatchVariables: false,
  alwaysStrict: false,
  types: ['*'],
  noUncheckedSideEffectImports: false,
  libReplacement: true,
  stableTypeOrdering: true,
};

describe('carrick#2019: TypeScript 5 values for the options a project leaves unset', () => {
  it('reads an empty tsconfig as TypeScript 5.9 does, with no error from the values it fills', () => {
    const { program, options } = programFor({});
    assert.deepStrictEqual(effective(options), TS5_EMPTY);
    assert.deepStrictEqual(optionErrors(program), []);
  });

  it('derives each unset option from the ones a project sets, as TypeScript 5.9 does', () => {
    // module nodenext: target ESNext, interop on, JSON modules resolve.
    assert.deepStrictEqual(effective(programFor({ compilerOptions: { module: 'nodenext' } }).options), {
      ...TS5_EMPTY,
      target: T.ESNext,
      module: M.NodeNext,
      moduleResolution: R.NodeNext,
      esModuleInterop: true,
      allowSyntheticDefaultImports: true,
      resolveJsonModule: true,
      resolvePackageJsonExports: true,
      useDefineForClassFields: true,
    });
    // An ES module target with no module: ES2015 modules, classic resolution.
    assert.deepStrictEqual(effective(programFor({ compilerOptions: { target: 'es2022' } }).options), {
      ...TS5_EMPTY,
      target: T.ES2022,
      module: M.ES2015,
      moduleResolution: R.Classic,
      useDefineForClassFields: true,
    });
    // Bundler resolution allows synthetic defaults and JSON modules.
    assert.deepStrictEqual(
      effective(programFor({ compilerOptions: { module: 'esnext', moduleResolution: 'bundler' } }).options),
      {
        ...TS5_EMPTY,
        module: M.ESNext,
        moduleResolution: R.Bundler,
        allowSyntheticDefaultImports: true,
        resolveJsonModule: true,
        resolvePackageJsonExports: true,
      }
    );
    // `strict` turns every flag it implies on, `alwaysStrict` with them.
    assert.deepStrictEqual(effective(programFor({ compilerOptions: { strict: true } }).options), {
      ...TS5_EMPTY,
      strictNullChecks: true,
      noImplicitAny: true,
      strictFunctionTypes: true,
      useUnknownInCatchVariables: true,
      alwaysStrict: true,
    });
  });

  it('keeps every value a project sets', () => {
    const { options } = programFor({
      compilerOptions: {
        target: 'es2020',
        module: 'esnext',
        moduleResolution: 'bundler',
        esModuleInterop: true,
        strict: false,
        strictNullChecks: true,
        types: ['node'],
        noUncheckedSideEffectImports: true,
      },
    });
    assert.strictEqual(options.target, T.ES2020);
    assert.strictEqual(options.module, M.ESNext);
    assert.strictEqual(options.moduleResolution, R.Bundler);
    assert.strictEqual(options.esModuleInterop, true);
    assert.strictEqual(options.strict, false);
    assert.strictEqual(options.strictNullChecks, true);
    assert.deepStrictEqual(options.types, ['node']);
    assert.strictEqual(options.noUncheckedSideEffectImports, true);
  });

  it('loads a tsconfig that sets options TypeScript 6 deprecates, with no error', () => {
    const { program } = programFor({
      compilerOptions: {
        target: 'es5',
        module: 'commonjs',
        moduleResolution: 'node',
        baseUrl: '.',
        paths: { '@lib/*': ['src/*'] },
        downlevelIteration: true,
        esModuleInterop: false,
        allowSyntheticDefaultImports: false,
        alwaysStrict: false,
      },
    });
    assert.deepStrictEqual(optionErrors(program), []);
  });

  it('writes a check tsconfig the fill leaves as it is', () => {
    const parsed = ts.parseJsonConfigFileContent(JSON.parse(CHECKER_TSCONFIG), ts.sys, os.tmpdir());
    // No probe files exist here (TS18003); any other error is the config's.
    assert.deepStrictEqual(parsed.errors.filter((d) => d.code !== 18003), []);
    assert.deepStrictEqual(sidecarCompilerOptions(parsed.options), parsed.options);
  });
});

interface InferRow {
  alias: string;
  type_string: string;
}

/** `init` on `root`, then one `infer` per batch, in order, in one process. */
async function inferInOneProcess(
  root: string,
  batches: Array<Array<Record<string, unknown>>>
): Promise<Map<string, string>> {
  const client = new SidecarClient();
  await client.start();
  try {
    const init = await client.send<{ status: string }>({ action: 'init', request_id: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready', JSON.stringify(init));
    const out = new Map<string, string>();
    let n = 0;
    for (const requests of batches) {
      const res = await client.send<{ inferred_types?: InferRow[] }>(
        {
          action: 'infer',
          request_id: `infer-${n++}`,
          requests: requests.map((r) => ({ ...r, file_path: path.join(root, r.file_path as string) })),
        },
        60000
      );
      for (const row of res.inferred_types ?? []) out.set(row.alias, row.type_string);
    }
    return out;
  } finally {
    await client.stop();
  }
}

describe('carrick#2019: an empty customer tsconfig, through the process', () => {
  const FILES = {
    'tsconfig.json': '{}\n',
    // A global only an automatically included `@types` package declares.
    'node_modules/@types/fixture-env/package.json': JSON.stringify({ name: '@types/fixture-env', version: '1.0.0', types: 'index.d.ts' }),
    'node_modules/@types/fixture-env/index.d.ts': 'declare const FIXTURE_ENV: { region: string };\n',
    'src/read.ts': [
      'export function maybe(x?: string) {',
      '  return x;',
      '}',
      'export function region() {',
      '  return FIXTURE_ENV;',
      '}',
      '',
    ].join('\n'),
  };

  it('types without strict null checks and with every @types package, as TypeScript 5 did', async () => {
    const root = writeRepo(FILES);
    const rows = await inferInOneProcess(root, [
      [
        { file_path: 'src/read.ts', line_number: 1, infer_kind: 'signature_return', alias: 'Maybe' },
        { file_path: 'src/read.ts', line_number: 4, infer_kind: 'signature_return', alias: 'Region' },
      ],
    ]);
    assert.strictEqual(rows.get('Maybe'), 'string');
    assert.strictEqual(rows.get('Region')?.replace(/\s+/g, ' '), '{ region: string; }');
  });

  it('captures under the same options', async () => {
    const root = writeRepo(FILES);
    const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2019-stub-'));
    tempRoots.push(outDir);
    const client = new SidecarClient();
    await client.start();
    try {
      const res = await client.send<{ result?: CaptureStubResult }>(
        {
          action: 'capture_v2',
          request_id: 'capture',
          repo_root: root,
          service_name: 'app',
          out_dir: outDir,
          anchors: [
            { kind: 'infer', alias: 'Maybe_Return', source_file: 'src/read.ts', anchor_origin: 'deterministic-infer', line_number: 2, expression_text: 'x' },
            { kind: 'infer', alias: 'Region_Return', source_file: 'src/read.ts', anchor_origin: 'deterministic-infer', line_number: 5, expression_text: 'FIXTURE_ENV' },
          ],
        },
        60000
      );
      assert.ok(res.result?.success, JSON.stringify(res));
      const surface = fs.readFileSync(path.join(outDir, 'types', 'surface.d.ts'), 'utf8').replace(/\s+/g, ' ');
      assert.match(surface, /export type Maybe_Return = string;/);
      assert.match(surface, /export type Region_Return = \{ region: string; \};/);
    } finally {
      await client.stop();
    }
  });
});

describe('carrick#2019: one process prints a type the same whatever it checked first', () => {
  const FILES = {
    'tsconfig.json': JSON.stringify({ compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler' } }),
    'src/pets.ts': 'export interface Dog { bark: true }\nexport interface Cat { meow: true }\n',
    'src/pick.ts': [
      "import type { Dog, Cat } from './pets';",
      'export function pick(x: number) {',
      '  return x ? ({} as Dog) : ({} as Cat);',
      '}',
      '',
    ].join('\n'),
    'src/handler.ts': [
      "import type { Dog, Cat } from './pets';",
      'export function onPet(pet: Cat | Dog) {',
      '  return pet;',
      '}',
      '',
    ].join('\n'),
    'src/keys-a.ts': [
      'export function a() {',
      "  const k = {} as { [P in 'n' | 'm']: number };",
      '  return { ...k };',
      '}',
      '',
    ].join('\n'),
    'src/keys-b.ts': [
      'export function b() {',
      "  const k = {} as { [P in 'm' | 'n']: number };",
      '  return { ...k };',
      '}',
      '',
    ].join('\n'),
  };
  const PET = { file_path: 'src/handler.ts', line_number: 2, infer_kind: 'function_param', param_name: 'pet', alias: 'Pet' };
  const KEYS_A = { file_path: 'src/keys-a.ts', line_number: 1, infer_kind: 'signature_return', alias: 'KeysA' };
  const PICK = { file_path: 'src/pick.ts', line_number: 2, infer_kind: 'signature_return', alias: 'Pick' };
  const KEYS_B = { file_path: 'src/keys-b.ts', line_number: 1, infer_kind: 'signature_return', alias: 'KeysB' };

  it('gives the union and the mapped keys one text, asked first or after the types that met them in another order', async () => {
    const root = writeRepo(FILES);
    const cold = await inferInOneProcess(root, [[PET, KEYS_A]]);
    const warm = await inferInOneProcess(root, [[PICK, KEYS_B], [PET, KEYS_A]]);
    assert.strictEqual(warm.get('Pet'), cold.get('Pet'));
    assert.strictEqual(warm.get('KeysA'), cold.get('KeysA'));
    // Both orders print the content order, not the order either file wrote.
    assert.strictEqual(cold.get('Pet'), 'Cat | Dog');
    assert.strictEqual(cold.get('KeysA')?.replace(/\s+/g, ' '), '{ m: number; n: number; }');
  });
});

describe('carrick#2019: every program the sidecar builds goes through sidecarCompilerOptions', () => {
  // dist/test/ -> the sidecar root -> src/.
  const SRC = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'src');

  function* sources(dir: string): Generator<string> {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const abs = path.join(dir, entry.name);
      if (entry.isDirectory()) yield* sources(abs);
      else if (entry.name.endsWith('.ts')) yield abs;
    }
  }

  /** Does `expr` call the helper, itself or through a variable the file declares with it? */
  function goesThroughHelper(expr: ts.Expression | undefined, file: ts.SourceFile): boolean {
    if (!expr) return false;
    if (expr.getText(file).includes('sidecarCompilerOptions(')) return true;
    if (!ts.isIdentifier(expr)) return false;
    let found = false;
    const visit = (node: ts.Node): void => {
      if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === expr.text) {
        found ||= node.initializer?.getText(file).includes('sidecarCompilerOptions(') ?? false;
      }
      ts.forEachChild(node, visit);
    };
    visit(file);
    return found;
  }

  /** The options argument of a `createProgram` call, positional or in its object form. */
  function programOptions(call: ts.CallExpression, file: ts.SourceFile): ts.Expression | undefined {
    const [first, second] = call.arguments;
    if (first && ts.isObjectLiteralExpression(first)) {
      return first.properties.find(
        (p): p is ts.PropertyAssignment => ts.isPropertyAssignment(p) && p.name.getText(file) === 'options'
      )?.initializer;
    }
    return second;
  }

  /** The `compilerOptions` a ts-morph `Project` is constructed with. */
  function projectOptions(created: ts.NewExpression, file: ts.SourceFile): ts.Expression | undefined {
    const [config] = created.arguments ?? [];
    if (!config || !ts.isObjectLiteralExpression(config)) return undefined;
    return config.properties.find(
      (p): p is ts.PropertyAssignment => ts.isPropertyAssignment(p) && p.name.getText(file) === 'compilerOptions'
    )?.initializer;
  }

  it('finds no ts.createProgram or ts-morph Project built without it', () => {
    const offenders: string[] = [];
    let sites = 0;
    for (const abs of sources(SRC)) {
      const file = ts.createSourceFile(abs, fs.readFileSync(abs, 'utf8'), ts.ScriptTarget.Latest, true);
      const where = (node: ts.Node): string =>
        `${path.relative(SRC, abs)}:${file.getLineAndCharacterOfPosition(node.getStart(file)).line + 1}`;
      const visit = (node: ts.Node): void => {
        if (ts.isCallExpression(node) && node.expression.getText(file).endsWith('createProgram')) {
          sites++;
          if (!goesThroughHelper(programOptions(node, file), file)) offenders.push(where(node));
        }
        if (ts.isNewExpression(node) && node.expression.getText(file) === 'Project') {
          sites++;
          if (!goesThroughHelper(projectOptions(node, file), file)) offenders.push(where(node));
        }
        ts.forEachChild(node, visit);
      };
      visit(file);
    }
    // Five ts-morph projects and six compiler programs when this was written.
    assert.ok(sites >= 11, `found ${sites} program sites; the walk lost some`);
    assert.deepStrictEqual(offenders, []);
  });
});
