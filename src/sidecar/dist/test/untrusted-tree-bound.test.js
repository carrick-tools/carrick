/**
 * carrick#2230 part 2: under CARRICK_UNTRUSTED_TREE the compiler reads no file
 * whose real path is outside the checkout.
 *
 * A scan on a machine that scans repositories it does not own reads only the
 * repository it was given. The fixture is generated here: a directory beside
 * the checkout holds a file carrying a sentinel, and the checkout reaches it
 * three ways. A tracked symbolic link, a relative import that climbs out, and
 * a tsconfig `include` that names the outside directory by absolute path. It
 * also holds an installed package whose link resolves INSIDE the checkout,
 * which must keep reading.
 *
 * The known positive comes first: with the switch unset the sentinel is read
 * (the capture publishes it, the loader's project holds it). Without that, a
 * refusal and a collapse look alike.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import { ProjectLoader } from '../src/project-loader.js';
import { UNTRUSTED_TREE_ENV } from '../src/capture/checkout.js';
const SENTINEL = 'SENTINEL_9f3c_outside';
const PKG_MARK = 'PKG_MARK_inside_link';
function walkText(dir) {
    let out = '';
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const abs = path.join(dir, entry.name);
        out += entry.isDirectory() ? walkText(abs) : fs.readFileSync(abs, 'utf8') + '\n';
    }
    return out;
}
describe('carrick#2230: an untrusted scan reads inside the checkout only', () => {
    let base;
    let checkout;
    let outside;
    const saved = process.env[UNTRUSTED_TREE_ENV];
    const write = (rel, text) => {
        fs.mkdirSync(path.dirname(path.join(checkout, rel)), { recursive: true });
        fs.writeFileSync(path.join(checkout, rel), text);
    };
    before(() => {
        base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2230b-'));
        outside = path.join(base, 'outside');
        checkout = path.join(base, 'checkout');
        fs.mkdirSync(outside, { recursive: true });
        fs.writeFileSync(path.join(outside, 'secret.ts'), `export interface Secret { ${SENTINEL}: string }\n`);
        fs.mkdirSync(path.join(checkout, '.git'), { recursive: true });
        write('package.json', JSON.stringify({ name: 'svc', private: true }));
        write('tsconfig.json', JSON.stringify({
            compilerOptions: { target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler', strict: true, skipLibCheck: true },
            include: ['src', outside],
        }));
        fs.mkdirSync(path.join(checkout, 'src'), { recursive: true });
        fs.symlinkSync(path.join(outside, 'secret.ts'), path.join(checkout, 'src/link.ts'));
        write('src/uses.ts', [
            "import type { Secret } from '../../outside/secret';",
            "import type { Secret as Linked } from './link';",
            "import type { Pkg } from 'pkg';",
            'export interface Wrapped { own: string; s: Secret; l: Linked; p: Pkg }',
            '',
        ].join('\n'));
        // An installed package: node_modules/pkg is a link that resolves inside the checkout.
        write('vendor/pkg/package.json', JSON.stringify({ name: 'pkg', version: '1.0.0', types: 'index.d.ts' }));
        write('vendor/pkg/index.d.ts', `export interface Pkg { ${PKG_MARK}: number }\n`);
        fs.mkdirSync(path.join(checkout, 'node_modules'), { recursive: true });
        fs.symlinkSync(path.join(checkout, 'vendor/pkg'), path.join(checkout, 'node_modules/pkg'));
    });
    after(() => {
        if (saved === undefined)
            delete process.env[UNTRUSTED_TREE_ENV];
        else
            process.env[UNTRUSTED_TREE_ENV] = saved;
        fs.rmSync(base, { recursive: true, force: true });
    });
    const capture = (name) => {
        const outDir = path.join(base, `stub-${name}`);
        const result = captureStub({
            repoRoot: checkout,
            serviceName: name,
            outDir,
            anchors: [
                {
                    kind: 'symbol',
                    alias: 'Endpoint_svc_Response',
                    symbol_name: 'Wrapped',
                    source_file: 'src/uses.ts',
                    anchor_origin: 'llm-symbol',
                },
            ],
        });
        const anyPaths = (result.aliases?.[0]?.any_provenance ?? []).map((p) => p.path).sort();
        return { text: JSON.stringify(result) + '\n' + walkText(outDir), anyPaths };
    };
    const wrappedMembers = () => {
        const loader = new ProjectLoader({ repoRoot: checkout });
        assert.ok(loader.load().success);
        const iface = loader
            .getProject()
            .getSourceFileOrThrow(path.join(checkout, 'src/uses.ts'))
            .getInterfaceOrThrow('Wrapped');
        return Object.fromEntries(iface.getProperties().map((p) => [p.getName(), p.getType().isAny() ? 'any' : p.getType().getText()]));
    };
    it('1. switch unset: the outside file is read (the known positive)', () => {
        delete process.env[UNTRUSTED_TREE_ENV];
        const open = capture('open');
        assert.ok(open.text.includes(SENTINEL), 'capture publishes the outside type');
        assert.deepStrictEqual(open.anyPaths, []);
        const members = wrappedMembers();
        assert.notStrictEqual(members.s, 'any');
        assert.notStrictEqual(members.l, 'any');
    });
    it('2. switch set: the capture publishes nothing from outside and still reads the installed package', () => {
        process.env[UNTRUSTED_TREE_ENV] = '1';
        const { text, anyPaths } = capture('bound');
        assert.ok(!text.includes(SENTINEL), 'sentinel reached the capture');
        assert.ok(text.includes('own: string'), 'the checkout is read');
        // The outside members read as unresolved; the package behind a link that
        // resolves inside the checkout does not.
        assert.deepStrictEqual(anyPaths, ['l', 's']);
    });
    it('3. switch set: the loader reads outside paths as missing, so their types are any', () => {
        process.env[UNTRUSTED_TREE_ENV] = '1';
        const members = wrappedMembers();
        assert.strictEqual(members.s, 'any');
        assert.strictEqual(members.l, 'any');
        assert.strictEqual(members.own, 'string');
        assert.match(members.p, /Pkg/, 'the installed package keeps its type');
        const loader = new ProjectLoader({ repoRoot: checkout });
        loader.load();
        for (const file of loader.getProject().getSourceFiles()) {
            assert.ok(!file.getFullText().includes(SENTINEL), `${file.getFilePath()} holds the sentinel`);
        }
    });
});
// carrick#2418: a referenced project is read from source, but never one whose
// real path is outside the checkout. Here the referenced package is a link
// that leaves the checkout; under the switch its config, outputs and source
// read as missing, so the reference stays unresolved.
describe('carrick#2418: a referenced project outside the checkout is not read', () => {
    const FAR_MARK = 'FAR_MARK_9f3c_outside_reference';
    let base;
    let service;
    const saved = process.env[UNTRUSTED_TREE_ENV];
    before(() => {
        base = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2418-bound-')));
        const outside = path.join(base, 'outside', 'linked');
        const checkout = path.join(base, 'checkout');
        const put = (abs, text) => {
            fs.mkdirSync(path.dirname(abs), { recursive: true });
            fs.writeFileSync(abs, text);
        };
        put(path.join(outside, 'package.json'), JSON.stringify({ name: '@acme/linked', version: '1.0.0', types: 'dist/index.d.ts' }));
        put(path.join(outside, 'tsconfig.json'), JSON.stringify({ compilerOptions: { composite: true, rootDir: 'src', outDir: 'dist', strict: true }, include: ['src'] }));
        put(path.join(outside, 'src', 'index.ts'), `export interface Far { ${FAR_MARK}: string }\n`);
        fs.mkdirSync(path.join(checkout, '.git'), { recursive: true });
        fs.mkdirSync(path.join(checkout, 'packages'), { recursive: true });
        fs.symlinkSync(outside, path.join(checkout, 'packages', 'linked'), 'dir');
        fs.mkdirSync(path.join(checkout, 'node_modules', '@acme'), { recursive: true });
        fs.symlinkSync(path.join('..', '..', 'packages', 'linked'), path.join(checkout, 'node_modules', '@acme', 'linked'), 'dir');
        service = path.join(checkout, 'services', 'api');
        put(path.join(service, 'package.json'), JSON.stringify({ name: 'api', version: '1.0.0', type: 'module' }));
        put(path.join(service, 'tsconfig.json'), JSON.stringify({
            compilerOptions: { module: 'nodenext', moduleResolution: 'nodenext', strict: true, skipLibCheck: true },
            include: ['src'],
            references: [{ path: '../../packages/linked' }],
        }));
        put(path.join(service, 'src', 'uses.ts'), "import type { Far } from '@acme/linked';\nexport interface Holds { far: Far }\n");
    });
    after(() => {
        if (saved === undefined)
            delete process.env[UNTRUSTED_TREE_ENV];
        else
            process.env[UNTRUSTED_TREE_ENV] = saved;
        fs.rmSync(base, { recursive: true, force: true });
    });
    const farText = () => {
        const loader = new ProjectLoader({ repoRoot: service });
        assert.ok(loader.load().success);
        const project = loader.getProject();
        const far = project
            .getSourceFileOrThrow(path.join(service, 'src', 'uses.ts'))
            .getInterfaceOrThrow('Holds')
            .getPropertyOrThrow('far')
            .getType();
        return { text: far.getText() + ' ' + far.getProperties().map((p) => p.getName()).join(','), files: project.getSourceFiles().map((f) => f.getFullText()) };
    };
    it('1. switch unset: the linked reference is read from source (the known positive)', () => {
        delete process.env[UNTRUSTED_TREE_ENV];
        assert.match(farText().text, new RegExp(FAR_MARK));
    });
    it('2. switch set: nothing outside is read and the reference stays unresolved', () => {
        process.env[UNTRUSTED_TREE_ENV] = '1';
        const { text, files } = farText();
        assert.ok(!text.includes(FAR_MARK), text);
        assert.ok(!files.some((file) => file.includes(FAR_MARK)), 'a program file holds the outside source');
        const result = captureStub({
            repoRoot: service,
            serviceName: 'api',
            outDir: path.join(base, 'stub'),
            anchors: [
                { kind: 'symbol', alias: 'Endpoint_far_Response', symbol_name: 'Holds', source_file: 'src/uses.ts', anchor_origin: 'llm-symbol' },
            ],
        });
        assert.ok(!(JSON.stringify(result) + walkText(path.join(base, 'stub'))).includes(FAR_MARK), 'the capture read outside');
    });
});
