/**
 * A declaration in an installed package is named in rows whatever the
 * install tool's layout (carrick#2599).
 *
 * Some resolvers serve a package from a cache outside the scanned repo whose
 * path has no `node_modules` segment (`<cache>/<registry>/<name>/<version>/…`),
 * so reading the package directory off the path finds nothing. The module
 * graph that resolved the file knows the package directory; `PathNames` asks
 * it after the root and the path rule, and names the file
 * `pkg:<name>/<path in the package>`, as it names a package installed above
 * the root.
 */
import { describe, it } from 'node:test';
import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { PathNames } from '../src/position-rows.js';
/** A repo, a package in a cache beside it with no `node_modules` on its path, and one installed above the repo. */
function layout() {
    const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-pkg-outside-')));
    const repo = path.join(base, 'repo');
    fs.mkdirSync(path.join(repo, 'src'), { recursive: true });
    const cacheDir = path.join(base, 'cache', 'registry.example', '@scope', 'widget', '1.2.3');
    fs.mkdirSync(path.join(cacheDir, 'dist', 'types'), { recursive: true });
    fs.writeFileSync(path.join(cacheDir, 'package.json'), JSON.stringify({ name: '@scope/widget', version: '1.2.3' }));
    const cached = path.join(cacheDir, 'dist', 'types', 'index.d.ts');
    fs.writeFileSync(cached, 'export declare function make(): void;\n');
    const hoistedDir = path.join(base, 'node_modules', 'flat');
    fs.mkdirSync(hoistedDir, { recursive: true });
    fs.writeFileSync(path.join(hoistedDir, 'package.json'), JSON.stringify({ name: 'flat', version: '4.5.6' }));
    const hoisted = path.join(hoistedDir, 'index.d.ts');
    fs.writeFileSync(hoisted, 'export declare const flat: number;\n');
    return { base, repo, cached, cacheDir, hoisted };
}
describe('PathNames: a package outside the root with no node_modules on its path (carrick#2599)', () => {
    it('names a file the module graph places in a package directory', () => {
        const { base, repo, cached, cacheDir } = layout();
        try {
            const paths = new PathNames(repo, (file) => file.startsWith(cacheDir + path.sep) ? { directory: cacheDir, name: '@scope/widget' } : undefined);
            assert.equal(paths.declaringFile(cached), 'pkg:@scope/widget/dist/types/index.d.ts');
            assert.equal(paths.scrubType(`import("${cached}").Widget`), 'import("pkg:@scope/widget/dist/types/index.d.ts").Widget');
        }
        finally {
            fs.rmSync(base, { recursive: true, force: true });
        }
    });
    it('names nothing the graph does not place, and nothing without a package.json', () => {
        const { base, repo, cached, cacheDir } = layout();
        try {
            assert.equal(new PathNames(repo).declaringFile(cached), null);
            const empty = path.join(base, 'cache', 'loose');
            fs.mkdirSync(empty, { recursive: true });
            const loose = path.join(empty, 'index.d.ts');
            fs.writeFileSync(loose, 'export {};\n');
            const paths = new PathNames(repo, (file) => file.startsWith(cacheDir + path.sep)
                ? { directory: cacheDir, name: '@scope/widget' }
                : { directory: empty, name: 'loose' });
            assert.equal(paths.declaringFile(loose), null);
        }
        finally {
            fs.rmSync(base, { recursive: true, force: true });
        }
    });
    it('leaves files inside the root and packages on a node_modules path named as before', () => {
        const { base, repo, hoisted } = layout();
        try {
            const own = path.join(repo, 'src', 'own.ts');
            fs.writeFileSync(own, 'export {};\n');
            const asked = [];
            const paths = new PathNames(repo, (file) => {
                asked.push(file);
                return { directory: path.dirname(file), name: 'graph-answer' };
            });
            assert.equal(paths.declaringFile(own), 'src/own.ts');
            assert.equal(paths.declaringFile(hoisted), 'pkg:flat/index.d.ts');
            assert.deepEqual(asked, [], 'the graph is asked only when the root and the path name nothing');
        }
        finally {
            fs.rmSync(base, { recursive: true, force: true });
        }
    });
});
