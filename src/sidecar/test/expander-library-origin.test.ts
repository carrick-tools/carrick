/**
 * carrick#1264 (third gate): the structural printer must decide library origin
 * from the PROGRAM, not from a `node_modules` path segment.
 *
 * A runtime that serves an installed package's types out of its own cache
 * resolves them to a path with no `node_modules` anywhere in it, and hands the
 * compiler the graph's `isExternalLibraryImport` verdict instead. A printer
 * that tests the path recognises nothing there and walks the package's
 * internals as if they were the user's own contract. What that costs is not
 * verbosity: an interface declared as `extends Array<T>` is an object type
 * with the whole array prototype on it, whose signatures carry `thisArg?: any`
 * — so the printed contract carries a top type that is not in the contract,
 * and the row is demoted to unresolved.
 *
 * The fixture is a real temp tree because both halves of the mechanism are the
 * compiler's: the resolver's verdict and the prototype members it materialises.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project, ts } from 'ts-morph';
import { expandTypeStructural } from '../src/type-structural-expander.js';

/** A checkout whose one dependency resolves out of a cache beside it. */
function cachedDependencyTree(): {
  project: Project;
  root: string;
  cleanup: () => void;
} {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-expander-root-')));
  const cache = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-expander-cache-')));
  fs.mkdirSync(path.join(root, '.git'));
  const write = (base: string, rel: string, text: string): string => {
    const abs = path.join(base, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
    return abs;
  };
  const dep = write(
    cache,
    'npm/registry/store/1.0.0/index.d.ts',
    [
      'export type Cell = string | number | null | CellList;',
      // The idiom: an interface that IS an array. Walked as an object it
      // prints the whole array prototype.
      'export interface CellList extends Array<Cell> {}',
    ].join('\n')
  );
  const user = write(
    root,
    'src/handler.ts',
    [
      'import type { Cell } from "store";',
      // The user's own contract, carrying one member the dependency declares.
      'export interface Payload { id: string; cells: Cell }',
      'export const row: Payload = null!;',
    ].join('\n')
  );
  const options: ts.CompilerOptions = {
    strict: true,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    target: ts.ScriptTarget.ES2022,
    types: [],
  };
  const project = new Project({
    compilerOptions: options,
    skipAddingFilesFromTsConfig: true,
    resolutionHost: () => ({
      // The shape a graph-backed resolver hands the compiler.
      resolveModuleNames: (names) =>
        names.map((name) =>
          name === 'store'
            ? {
                resolvedFileName: dep,
                isExternalLibraryImport: true,
                extension: ts.Extension.Dts,
              }
            : undefined
        ),
    }),
  });
  // Only the user's file is a root: a root file is the program's own source
  // by construction, and the dependency has to arrive through resolution for
  // the resolver's verdict to be recorded on it.
  project.addSourceFileAtPath(user);
  return {
    project,
    root,
    cleanup: () => {
      fs.rmSync(root, { recursive: true, force: true });
      fs.rmSync(cache, { recursive: true, force: true });
    },
  };
}

describe('structural printer library origin comes from the program (carrick#1264)', () => {
  it('keeps a cached dependency type by name instead of walking its prototype', () => {
    const { project, root, cleanup } = cachedDependencyTree();
    try {
      const declaration = project
        .getSourceFileOrThrow((file) => file.getFilePath().endsWith('handler.ts'))
        .getVariableDeclarationOrThrow('row');
      const dependency = declaration.getType().getProperty('cells')!;
      const dependencyFile: string | undefined = dependency
        .getTypeAtLocation(declaration)
        .getUnionTypes()
        .map((member) => member.getSymbol()?.getDeclarations()?.[0]?.getSourceFile().getFilePath())
        .find((file) => file !== undefined);

      // The fixture exercises the compiler clause and nothing else: no path
      // test can recognise this dependency.
      assert.ok(dependencyFile, 'the union carries a declared member');
      assert.ok(
        !dependencyFile.includes('/node_modules/'),
        `the fixture dependency must not resolve through node_modules: ${dependencyFile}`
      );
      assert.strictEqual(
        project
          .getProgram()
          .compilerObject.isSourceFileFromExternalLibrary(
            project.getSourceFileOrThrow(dependencyFile).compilerNode
          ),
        true,
        'the program must mark the fixture dependency external'
      );

      const printed = expandTypeStructural(declaration.getType(), {
        program: project.getProgram().compilerObject,
        repoRoot: root,
      });

      // The user's own interface is still inlined...
      assert.match(printed, /id: string/, printed);
      // ...and the dependency's array-like member is not walked into the
      // array prototype, so no signature `any` reaches the contract.
      assert.ok(
        !/\bany\b/.test(printed),
        `the printed contract must carry no top type from library machinery:\n${printed}`
      );
      assert.ok(
        !/thisArg/.test(printed),
        `the array prototype must not be printed as the contract:\n${printed}`
      );
    } finally {
      cleanup();
    }
  });
});
