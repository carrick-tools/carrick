import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { loaderErrors, nativeErrors, nativeTsc, scratch, writeShadow, type Shadow } from './held-shadow-support.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.resolve(here, '..', '..', 'test', 'fixtures', 'held-node');

describe('write_shadow (held compiler program)', () => {
  it('(a) vendors a node_modules file and never counts it as own', () => {
    const out = scratch('held-shadow-a-');
    const shadow = writeShadow(FIXTURE, out);
    assert.deepEqual(shadow.own_files, ['src/helper.ts', 'src/libuse.ts', 'src/orders.ts']);
    assert.ok(shadow.vendored > 0);
    assert.ok(fs.existsSync(path.join(out, 'w', 'src', 'orders.ts')));
    assert.ok(!fs.existsSync(path.join(out, 'w', 'node_modules')));
    const vendoredHono = path.join(out, 'v', FIXTURE, 'node_modules', 'hono', 'index.d.ts');
    assert.ok(fs.existsSync(vendoredHono), vendoredHono);
    assert.deepEqual(shadow.path_conflicts, []);
    assert.ok(shadow.bare_specifiers.includes('hono'));
  });

  it('(b) names @types/node in files when the repo sets no types', () => {
    const out = scratch('held-shadow-b-');
    const shadow = writeShadow(FIXTURE, out);
    const config = JSON.parse(fs.readFileSync(shadow.tsconfig, 'utf8')) as { files: string[]; compilerOptions: { lib: string[] } };
    assert.ok(config.files.some((f) => f.endsWith(path.join('@types', 'node', 'index.d.ts'))), config.files.join('\n'));
    // The default library files are named, so DOM and es2022 members still resolve.
    assert.ok(config.compilerOptions.lib.includes('dom'));
    assert.ok(config.compilerOptions.lib.every((n) => !n.endsWith('.full')));
  });

  it('(c) the native compiler reports the loader program\'s own-file errors, none extra', () => {
    const out = scratch('held-shadow-c-');
    const shadow = writeShadow(FIXTURE, out);
    const expected = loaderErrors(FIXTURE);
    assert.deepEqual(expected, ['src/orders.ts:8:TS2322', 'src/orders.ts:9:TS2322'], 'the fixture\'s two deliberate errors');
    assert.deepEqual(nativeErrors(shadow.tsconfig, path.join(out, 'w')), expected);
  });

  it('(d) carries none of the options the native compiler refuses', () => {
    const repo = scratch('held-shadow-d-repo-');
    fs.mkdirSync(path.join(repo, 'src'));
    fs.writeFileSync(
      path.join(repo, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { baseUrl: '.', moduleResolution: 'node', module: 'commonjs', target: 'es5', esModuleInterop: false, lib: ['es2015'], strict: true, noEmit: true },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repo, 'src', 'a.ts'), 'export const a: number = 1;\n');
    const out = scratch('held-shadow-d-out-');
    const shadow = writeShadow(repo, out);
    const options = (JSON.parse(fs.readFileSync(shadow.tsconfig, 'utf8')) as { compilerOptions: Record<string, unknown> }).compilerOptions;
    assert.equal('baseUrl' in options, false);
    assert.equal('esModuleInterop' in options, false);
    assert.equal(options.moduleResolution, 'bundler');
    assert.notEqual(String(options.target).toLowerCase(), 'es5');
    assert.deepEqual(nativeErrors(shadow.tsconfig, path.join(out, 'w')), []);
  });

  it('(e) says whether an own file imports hono', () => {
    assert.equal(writeShadow(FIXTURE, scratch('held-shadow-e1-')).hono, true);
    const repo = scratch('held-shadow-e-repo-');
    fs.mkdirSync(path.join(repo, 'src'));
    fs.writeFileSync(path.join(repo, 'tsconfig.json'), JSON.stringify({ compilerOptions: { strict: true, noEmit: true }, include: ['src'] }));
    fs.writeFileSync(path.join(repo, 'src', 'a.ts'), 'export const a: number = 1;\n');
    assert.equal(writeShadow(repo, scratch('held-shadow-e2-')).hono, false);
  });

  it('(f) answers the same files from two working directories', () => {
    const elsewhere = scratch('held-shadow-f-cwd-');
    const first = writeShadow(FIXTURE, scratch('held-shadow-f1-'), os.tmpdir());
    const second = writeShadow(FIXTURE, scratch('held-shadow-f2-'), elsewhere);
    const filesOf = (s: Shadow): string[] => {
      const out = path.dirname(path.dirname(s.tsconfig));
      return (JSON.parse(fs.readFileSync(s.tsconfig, 'utf8')) as { files: string[] }).files.map((f) => path.relative(out, f)).sort();
    };
    assert.equal(filesOf(first).length > 3, true);
    assert.deepEqual(first.own_files, second.own_files);
    assert.equal(first.vendored, second.vendored);
    assert.deepEqual(filesOf(first), filesOf(second));
  });

  it('reports the native compiler path, or null where it is not installed', () => {
    const shadow = writeShadow(FIXTURE, scratch('held-shadow-g-'));
    assert.equal(shadow.native_tsc, nativeTsc());
  });
});
