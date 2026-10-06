/**
 * carrick#2031: a receiver whose type is a global interface that several
 * declaration files merge (the compiler's default library declares it, a
 * typings package augments it) is attributed to one package, whatever the
 * process did before.
 *
 * On TypeScript 6 the merged symbol's declarations come back in another order
 * once the program has been rebuilt (any edit, such as the retype's rewrite
 * or the inferrer's marking pass, rebuilds it): the augmentation's
 * declaration comes first. Taking the first declaration therefore named the
 * typings package after a rebuild and the compiler's library before it. The
 * rule: where the default library declares the interface, the package is the
 * library's; otherwise it is the declaring file whose path sorts first.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { TypeInferrer } from '../src/type-inferrer.js';
import { sidecarCompilerOptions } from '../src/capture/compiler-options.js';

const tempRoots: string[] = [];
after(() => {
  for (const root of tempRoots) fs.rmSync(root, { recursive: true, force: true });
});

const SEND = 'export function send() {\n  const fd = new FormData();\n  fd.append("a", "b");\n  return fd;\n}\n';

function writeRepo(files: Record<string, string>): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2031-')));
  tempRoots.push(root);
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return root;
}

function projectOf(root: string): Project {
  const tsconfig = path.join(root, 'tsconfig.json');
  const loaded = new Project({ tsConfigFilePath: tsconfig, skipAddingFilesFromTsConfig: true });
  return new Project({
    tsConfigFilePath: tsconfig,
    compilerOptions: sidecarCompilerOptions(loaded.getCompilerOptions()),
  });
}

/** The receiver of `fd.append(...)`, asked of `inferrer`. */
function receiverPackage(inferrer: TypeInferrer, root: string): string | undefined {
  const file = path.join(root, 'src/send.ts');
  const text = fs.readFileSync(file, 'utf8');
  const start = text.indexOf('fd.append(');
  const end = text.indexOf(';', start);
  const answer = inferrer.infer([
    { file_path: file, line_number: 3, span_start: start, span_end: end, infer_kind: 'receiver_type', alias: 'fd' },
  ]).inferred_types?.find((row) => row.alias === 'fd');
  assert.strictEqual(answer?.type_string, 'FormData');
  return answer?.declaring_package;
}

/** Rebuild the program: an edit to an unrelated file, then its restore. */
function rebuild(project: Project, root: string): void {
  const other = project.getSourceFileOrThrow(path.join(root, 'src/other.ts'));
  other.replaceWithText('export const two = 3;\n');
  project.getProgram();
  other.replaceWithText('export const two = 2;\n');
}

describe('carrick#2031: a merged global interface names one declaring package', () => {
  it('names the default library for a library interface a typings package augments, before and after a rebuild', () => {
    const root = writeRepo({
      'tsconfig.json': JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['dom', 'es2022'] },
        include: ['src'],
      }),
      'node_modules/@types/augmenting/package.json': JSON.stringify({ name: '@types/augmenting', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/@types/augmenting/index.d.ts': '/// <reference path="global.d.ts" />\nexport declare const marker: number;\n',
      'node_modules/@types/augmenting/global.d.ts': 'interface FormData {}\n',
      'src/send.ts': SEND,
      'src/other.ts': 'export const two = 2;\n',
    });
    const project = projectOf(root);
    const inferrer = new TypeInferrer({ project, repoRoot: root });
    const cold = receiverPackage(inferrer, root);
    rebuild(project, root);
    const warm = receiverPackage(inferrer, root);
    assert.strictEqual(cold, 'typescript');
    assert.strictEqual(warm, cold);
  });

  it('names the declaring file whose path sorts first when no library declares the interface', () => {
    const root = writeRepo({
      'tsconfig.json': JSON.stringify({
        // The later path is loaded first, so it merges first.
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['es2022'], types: ['zeta-forms', 'alpha-forms'] },
        include: ['src'],
      }),
      'node_modules/@types/zeta-forms/package.json': JSON.stringify({ name: '@types/zeta-forms', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/@types/zeta-forms/index.d.ts':
        'interface FormData { append(name: string, value: string): void }\ndeclare var FormData: { new (): FormData };\n',
      'node_modules/@types/alpha-forms/package.json': JSON.stringify({ name: '@types/alpha-forms', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/@types/alpha-forms/index.d.ts': 'interface FormData { size?: number }\n',
      'src/send.ts': SEND,
      'src/other.ts': 'export const two = 2;\n',
    });
    const project = projectOf(root);
    const inferrer = new TypeInferrer({ project, repoRoot: root });
    const cold = receiverPackage(inferrer, root);
    rebuild(project, root);
    const warm = receiverPackage(inferrer, root);
    assert.strictEqual(cold, '@types/alpha-forms');
    assert.strictEqual(warm, cold);
  });
});
