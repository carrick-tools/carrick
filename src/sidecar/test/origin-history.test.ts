/**
 * carrick#1731, outside Deno: a dependency linked from a directory outside
 * the checkout keeps reading as library origin after the project is edited.
 *
 * ts-morph rebuilds the program after any edit to the project with every file
 * it has loaded as a root file, and the compiler marks no root file as an
 * external-library import. A dependency installed under `node_modules` is
 * still recognised by its path. One linked from outside the checkout (`npm
 * link`, a `link:` or `file:` dependency) resolves to its real path, which has
 * no `node_modules` segment, so the compiler's flag was the only thing that
 * said it was a library, and the first edit made the printer inline it.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { ProjectLoader } from '../src/project-loader.js';
import { TypeBundler } from '../src/bundler.js';

describe('carrick#1731 library origin survives an edit to the project', () => {
  it('keeps a dependency linked from outside the checkout by name in a tsconfig project', () => {
    const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-origin-history-root-')));
    const outside = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-origin-history-linked-')));
    const write = (base: string, rel: string, text: string): string => {
      const abs = path.join(base, rel);
      fs.mkdirSync(path.dirname(abs), { recursive: true });
      fs.writeFileSync(abs, text);
      return abs;
    };
    try {
      fs.mkdirSync(path.join(root, '.git'));
      write(root, 'tsconfig.json', JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022' },
        include: ['src'],
      }));
      write(outside, 'package.json', JSON.stringify({ name: 'linked', version: '1.0.0', types: 'index.d.ts' }));
      // The package's own relative import: the compiler marks the file it
      // reaches as external too, because it is reached from an external file.
      write(outside, 'index.d.ts', 'export type { LibThing } from "./thing";\n');
      const thing = write(outside, 'thing.d.ts', 'export interface LibThing { x: number; y: string }\n');
      fs.mkdirSync(path.join(root, 'node_modules'));
      fs.symlinkSync(outside, path.join(root, 'node_modules', 'linked'), 'dir');
      const main = write(root, 'src/main.ts', [
        'import type { LibThing } from "linked";',
        'export interface Wrapped { thing: LibThing; id: string }',
      ].join('\n'));

      const loader = new ProjectLoader({ repoRoot: root });
      assert.strictEqual(loader.load().success, true);
      const project = loader.getProject();
      const bundler = new TypeBundler({ project, repoRoot: root });
      const bundle = () =>
        bundler.bundle([{ symbol_name: 'Wrapped', source_file: main, alias: 'WrappedAlias' }]).dts_content;

      const declaring = project.getSourceFileOrThrow(thing);
      assert.doesNotMatch(declaring.getFilePath(), /\/node_modules\//, 'the linked package resolves to its real path');
      assert.strictEqual(project.getProgram().compilerObject.isSourceFileFromExternalLibrary(declaring.compilerNode), true);
      const fresh = bundle();
      assert.match(fresh ?? '', /thing: LibThing;/, 'a library type stays by name');

      project.removeSourceFile(project.createSourceFile(path.join(root, 'src', '__probe.ts'), 'export {};\n'));
      assert.strictEqual(
        project.getProgram().compilerObject.isSourceFileFromExternalLibrary(declaring.compilerNode),
        false,
        'the fixture must reproduce the compiler forgetting the flag, or this test proves nothing'
      );
      assert.strictEqual(bundle(), fresh, 'the bundle after an edit');
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
      fs.rmSync(outside, { recursive: true, force: true });
    }
  });
});
