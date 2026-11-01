/**
 * carrick#3146: every program a loader project builds has the same roots, and
 * a rebuild after an edit that changes no import reuses the old program.
 *
 * ts-morph hands the compiler every file it has loaded as a root, so a
 * project's roots grew with each rebuild and a call's program depended on how
 * many rebuilds its process had done before it. The roots are now the first
 * build's, plus every file added through the project API.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import type { Project } from 'ts-morph';
import { ProjectLoader } from '../src/project-loader.js';
import { replaceFileText } from '../src/program-files.js';

const PACKAGE_BOX = { name: 'box', version: '1.0.0', types: 'index.d.ts' };
const BOX_DTS = "import type { Filler } from 'filler';\nexport interface Box {\n  filler: Filler;\n}\n";

const FILES: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: { target: 'ES2022', module: 'NodeNext', moduleResolution: 'NodeNext', strict: true, skipLibCheck: true },
    include: ['src/**/*'],
  }),
  'src/index.ts': "import { shout } from '../shared/shout.js';\nexport const ready = shout('ok');\n",
  'src/top.ts': "export type { Box } from 'box';\n",
  // Outside `include`: ts-morph marks it in-project when it resolves the import.
  'shared/shout.ts': 'export function shout(s: string): string { return s.toUpperCase(); }\n',
  // Outside `include`, imported by nothing.
  'tools/unpack.ts': "import type { Box } from 'crate';\nexport function unpack(b: Box) {\n  return b.filler;\n}\n",
  'node_modules/box/package.json': JSON.stringify(PACKAGE_BOX),
  'node_modules/box/index.d.ts': BOX_DTS,
  'node_modules/crate/package.json': JSON.stringify({ name: 'crate', version: '1.0.0', types: 'index.d.ts' }),
  'node_modules/crate/index.d.ts': "export type { Box } from 'box';\n",
  // A second copy of the same version, nested under the package that uses it.
  'node_modules/crate/node_modules/box/package.json': JSON.stringify(PACKAGE_BOX),
  'node_modules/crate/node_modules/box/index.d.ts': BOX_DTS,
  'node_modules/crate/node_modules/filler/package.json': JSON.stringify({ name: 'filler', version: '1.0.0', types: 'index.d.ts' }),
  'node_modules/crate/node_modules/filler/index.d.ts': 'export interface Filler {\n  grams: number;\n}\n',
};

function writeTree(files: Record<string, string>): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-program-roots-')));
  fs.mkdirSync(path.join(root, '.git'));
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return root;
}

const roots = (project: Project): string[] => [...project.getProgram().compilerObject.getRootFileNames()];
const files = (project: Project): string[] => project.getProgram().compilerObject.getSourceFiles().map((file) => file.fileName);
function cycles(project: Project, indexPath: string, count: number): void {
  const original = project.getSourceFileOrThrow(indexPath).getFullText();
  const program = files(project);
  const first = roots(project);
  for (let i = 0; i < count; i++) {
    replaceFileText(project.getSourceFileOrThrow(indexPath), `${original}\nexport const probe${i} = ${i};\n`);
    assert.deepStrictEqual(roots(project), first, `roots after edit ${i}`);
    assert.deepStrictEqual(files(project), program, `files after edit ${i}`);
    replaceFileText(project.getSourceFileOrThrow(indexPath), original);
    assert.deepStrictEqual(roots(project), first, `roots after restore ${i}`);
    assert.deepStrictEqual(files(project), program, `files after restore ${i}`);
  }
}

describe('carrick#3146 a loader project keeps its roots', () => {
  it('keeps the first build\'s roots and files through edits and restores', () => {
    const root = writeTree(FILES);
    try {
      const loader = new ProjectLoader({ repoRoot: root });
      assert.strictEqual(loader.load().success, true);
      const project = loader.getProject();
      const first = roots(project);
      assert.ok(first.includes(path.join(root, 'src/index.ts')));
      assert.ok(first.includes(path.join(root, 'src/top.ts')));
      assert.ok(!first.some((file) => file.includes('/node_modules/')), first.join('\n'));
      assert.ok(!first.includes(path.join(root, 'shared/shout.ts')), first.join('\n'));
      cycles(project, path.join(root, 'src/index.ts'), 3);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('adds a file to the roots once, and later edits change neither roots nor files', () => {
    const root = writeTree(FILES);
    try {
      const loader = new ProjectLoader({ repoRoot: root });
      assert.strictEqual(loader.load().success, true);
      const project = loader.getProject();
      const first = roots(project);
      const unpack = path.join(root, 'tools/unpack.ts');
      project.addSourceFileAtPath(unpack);
      assert.deepStrictEqual(roots(project), [...first, unpack]);
      cycles(project, path.join(root, 'src/index.ts'), 3);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
