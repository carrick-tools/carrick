/**
 * carrick#1604: a service with no config at or above its root types each file
 * under the config an editor would pick for it: the nearest `tsconfig.json`,
 * then `jsconfig.json`, in each directory from the file's up to the service
 * root.
 *
 * Before, such a service was typed as one default project under NodeNext
 * resolution, so a project that keeps its config in a subfolder (or a
 * `jsconfig.json`) read every alias and extensionless import as unresolved.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { ProjectLoader } from '../src/project-loader.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURES = path.join(__dirname, '..', '..', '..', '..', 'tests', 'fixtures');

const tempRoots: string[] = [];
after(() => {
  for (const root of tempRoots) fs.rmSync(root, { recursive: true, force: true });
});

/** The fixture's repo, copied so nothing is written into the checkout. */
function copyFixture(name: string): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1604-')));
  tempRoots.push(root);
  fs.cpSync(path.join(FIXTURES, name, 'repo'), root, { recursive: true });
  return root;
}

/** A repo laid out from `files` (repo-relative paths to text). */
function writeRepo(files: Record<string, string>): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1604-')));
  tempRoots.push(root);
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return root;
}

function load(root: string): ProjectLoader {
  const loader = new ProjectLoader({ repoRoot: root, scanRoot: root });
  assert.strictEqual(loader.load().success, true);
  return loader;
}

/** Every relative or alias import of `file` in its owner's program, resolved or not. */
function importsOf(loader: ProjectLoader, root: string, rel: string): Array<{ spec: string; resolved: boolean }> {
  const file = path.join(root, rel);
  const project = loader.getProjectFor(loader.projectKeyFor(file));
  const source = project.getSourceFile(file) ?? project.addSourceFileAtPath(file);
  return source
    .getImportDeclarations()
    .map((d) => ({ spec: d.getModuleSpecifierValue(), resolved: d.getModuleSpecifierSourceFile() !== undefined }));
}

describe('a service with no config at its root', () => {
  it('types each file under the nearest config on its way up, so every import resolves', () => {
    const root = copyFixture('configs-below-root');
    const loader = load(root);
    const web = path.join(root, 'web', 'tsconfig.json');
    const cli = path.join(root, 'cli', 'tsconfig.json');
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'web/src/api/accounts.ts')), web);
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'web/src/lib/http.ts')), web);
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'cli/src/main.ts')), cli);
    // Relative paths name the same owner.
    assert.strictEqual(loader.projectKeyFor('web/src/api/accounts.ts'), web);
    // No file, or a file no config is above, is the default project's.
    assert.strictEqual(loader.projectKeyFor(undefined), '');
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'scripts/build.ts')), '');

    for (const rel of ['web/src/api/accounts.ts', 'cli/src/main.ts']) {
      const imports = importsOf(loader, root, rel);
      assert.ok(imports.length > 0, rel);
      assert.deepStrictEqual(
        imports.filter((i) => !i.resolved),
        [],
        `${rel} has imports that do not resolve`
      );
    }
    // The two projects keep their own module modes.
    const options = (config: string) => loader.getProjectFor(config).getCompilerOptions();
    assert.notStrictEqual(options(web).moduleResolution, options(cli).moduleResolution);
  });

  it('reads a jsconfig.json, and loads no installed package source for it', () => {
    const root = copyFixture('jsconfig-root');
    const loader = load(root);
    const jsconfig = path.join(root, 'jsconfig.json');
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'src/page.js')), jsconfig);
    const imports = importsOf(loader, root, 'src/page.js');
    assert.strictEqual(imports.length, 2);
    assert.deepStrictEqual(imports.filter((i) => !i.resolved), []);
    assert.strictEqual(loader.getProjectFor(jsconfig).getCompilerOptions().maxNodeModuleJsDepth, 0);
  });

  it('keeps a depth the jsconfig states', () => {
    const root = writeRepo({
      'jsconfig.json': JSON.stringify({ compilerOptions: { maxNodeModuleJsDepth: 3 } }),
      'src/a.js': 'export const a = 1;\n',
    });
    const loader = load(root);
    const key = loader.projectKeyFor(path.join(root, 'src/a.js'));
    assert.strictEqual(key, path.join(root, 'jsconfig.json'));
    assert.strictEqual(loader.getProjectFor(key).getCompilerOptions().maxNodeModuleJsDepth, 3);
  });

  it('prefers a tsconfig.json to a jsconfig.json in the same directory, and the nearer directory to either', () => {
    const root = writeRepo({
      'jsconfig.json': '{}',
      'pkg/tsconfig.json': '{}',
      'pkg/jsconfig.json': '{}',
      'pkg/src/a.ts': 'export const a = 1;\n',
      'other/jsconfig.json': '{}',
      'other/b.js': 'export const b = 1;\n',
      'node_modules/dep/index.js': 'export const d = 1;\n',
    });
    const loader = load(root);
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'pkg/src/a.ts')), path.join(root, 'pkg/tsconfig.json'));
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'other/b.js')), path.join(root, 'other/jsconfig.json'));
    // The search stops at node_modules: the root's jsconfig is not above it.
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'node_modules/dep/index.js')), '');
  });

  it('hands a file the tsconfig lists nowhere to the jsconfig beside it', () => {
    const root = writeRepo({
      'web/tsconfig.json': JSON.stringify({ include: ['src/**/*.ts'] }),
      'web/jsconfig.json': '{}',
      'web/src/a.js': 'export const a = 1;\n',
      'web/src/b.ts': 'export const b = 1;\n',
    });
    const loader = load(root);
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'web/src/a.js')), path.join(root, 'web/jsconfig.json'));
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'web/src/b.ts')), path.join(root, 'web/tsconfig.json'));
  });

  it('keeps a nested config\'s files out of the default project', () => {
    const root = writeRepo({
      'src/top.ts': 'export const top = 1;\n',
      'src/pkg/tsconfig.json': '{}',
      'src/pkg/inner.ts': 'export const inner = 1;\n',
    });
    const loader = load(root);
    const files = loader
      .getProject()
      .getSourceFiles()
      .map((f) => path.relative(root, f.getFilePath()));
    assert.ok(files.includes('src/top.ts'), files.join());
    assert.ok(!files.includes('src/pkg/inner.ts'), files.join());
  });
});

describe('a service whose config the loader finds', () => {
  // carrick#3497 ruling R-N1: the nearest config inside the service whose
  // program lists the file types it, as the language service picks it; the
  // service config takes what no nearer config lists.
  it('types a nested config\'s files under it and keeps the rest in the default project', () => {
    const root = writeRepo({
      'tsconfig.json': JSON.stringify({ include: ['src', 'nested'] }),
      'src/a.ts': 'export const a = 1;\n',
      'nested/tsconfig.json': '{}',
      'nested/b.ts': 'export const b = 1;\n',
      'src/only/tsconfig.json': JSON.stringify({ include: ['listed'] }),
      'src/only/c.ts': 'export const c = 1;\n',
      'src/lib/tsconfig.json': '{}',
      'src/lib/e.js': 'export const e = 1;\n',
    });
    const loader = load(root);
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'src/a.ts')), '');
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'nested/b.ts')), path.join(root, 'nested/tsconfig.json'));
    // A config that does not list the file is passed over for one further up.
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'src/only/c.ts')), '');
    // So is one without allowJs, for a JavaScript file.
    assert.strictEqual(loader.projectKeyFor(path.join(root, 'src/lib/e.js')), '');
  });
});
