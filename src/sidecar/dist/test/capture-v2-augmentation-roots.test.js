/**
 * carrick#2317: the program a capture reads its anchors in holds the files of
 * the service that declare a module or global augmentation.
 *
 * A service may declare a member on a library's type in a file its handlers
 * never import (`declare module 'lib' { interface Instance { db: Db } }` in
 * the entry file, read as `app.db` in every route file). The service's own
 * compile sees it, because the tsconfig lists the file. A program rooted at
 * the surface entry and the anchors' files does not, so the member read `any`
 * there and so did everything read off it.
 *
 * The cases:
 *  1. one project: a module augmentation and a `declare global`, each in a
 *     file no anchored file imports, with the importing handler as control;
 *  2. several owning projects: each owner's anchors see that owner's
 *     augmentations and no other project's;
 *  3. the untrusted-tree read bound: an augmentation file outside the
 *     checkout is not read, not even to look for the declaration;
 *  4. a file that declares no augmentation is not made a root.
 *
 * The Deno path is in `deno-project.test.ts`, the heap a capture holds in
 * `capture-v2-emit-program-released.test.ts`.
 */
import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import fsModule from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub } from '../src/capture/index.js';
import { UNTRUSTED_TREE_ENV } from '../src/capture/checkout.js';
const roots = [];
after(() => {
    for (const root of roots)
        fs.rmSync(root, { recursive: true, force: true });
});
function tree(prefix, files) {
    const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
    roots.push(root);
    writeInto(root, files);
    return root;
}
function writeInto(root, files) {
    for (const [rel, text] of Object.entries(files)) {
        const abs = path.join(root, rel);
        fs.mkdirSync(path.dirname(abs), { recursive: true });
        fs.writeFileSync(abs, text);
    }
}
const OPTIONS = { target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler', strict: true, skipLibCheck: true };
/** An installed library whose instance type the service adds members to. */
const LIBRARY = {
    'node_modules/server-lib/package.json': JSON.stringify({ name: 'server-lib', version: '1.0.0', types: 'index.d.ts' }),
    'node_modules/server-lib/index.d.ts': 'export interface ServerInstance { get(path: string, handler: () => unknown): void }\n',
};
const DB_TS = [
    'export interface Item { id: string; price: number }',
    'export interface Database { all(): Item[]; first(): Item | undefined }',
    '',
].join('\n');
/** Reads `app.db` and imports nothing that declares it. */
const ITEMS_TS = [
    "import type { ServerInstance } from 'server-lib';",
    'function format(item: { id: string; price: number }) { return { id: item.id, label: `${item.price}` }; }',
    'export function listItems(app: ServerInstance) {',
    '  return app.db.all();',
    '}',
    'export function itemRoutes(app: ServerInstance) {',
    "  app.get('/items', () => {",
    '    const rows = app.db.all();',
    '    return rows.map(format);',
    '  });',
    '}',
    '',
].join('\n');
const lineOf = (text, needle) => {
    const index = text.split('\n').findIndex((line) => line.includes(needle));
    assert.ok(index >= 0, `fixture line not found: ${needle}`);
    return index + 1;
};
function inferAnchor(alias, file, text, expression) {
    return {
        kind: 'infer',
        alias,
        source_file: file,
        anchor_origin: 'deterministic-infer',
        expression_text: expression,
        line_number: lineOf(text, expression),
    };
}
function surfaceOf(result) {
    return fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
}
/** The declared text of one alias of the surface, on one line. */
function aliasText(result, alias) {
    const text = surfaceOf(result);
    const source = ts.createSourceFile('surface.d.ts', text, ts.ScriptTarget.Latest, true);
    const declared = source.statements.find((stmt) => ts.isTypeAliasDeclaration(stmt) && stmt.name.text === alias);
    assert.ok(declared, `${alias} not in the surface:\n${text}`);
    return declared.type.getText(source).replace(/\s+/g, ' ');
}
function record(result, alias) {
    const found = result.aliases.find((a) => a.alias === alias);
    assert.ok(found, `${alias} has no record`);
    return found;
}
/** The alias is the row type, checked clean, with no top type anywhere in it. */
function assertTyped(result, alias, members) {
    const found = record(result, alias);
    const text = aliasText(result, alias);
    assert.strictEqual(found.self_check, 'ok', `${alias}: ${JSON.stringify(found)}\n${surfaceOf(result)}`);
    assert.strictEqual(found.top_type_at_self_check, false, `${alias}: ${text}`);
    assert.strictEqual(found.any_provenance, undefined, `${alias}: ${JSON.stringify(found.any_provenance)}`);
    for (const member of members)
        assert.match(text, member, `${alias}: ${text}`);
}
/** The alias reads as a top type: the capture did not see the member's declaration. */
function assertUntyped(result, alias) {
    const found = record(result, alias);
    const text = aliasText(result, alias);
    assert.ok(found.top_type_at_self_check || (found.any_provenance ?? []).length > 0, `${alias} reads as typed: ${text} ${JSON.stringify(found)}`);
    assert.doesNotMatch(text, /Item|price/, `${alias}: ${text}`);
}
describe('carrick#2317: one project', () => {
    const serviceFiles = () => ({
        ...LIBRARY,
        'package.json': JSON.stringify({ name: 'shop', version: '1.0.0', dependencies: { 'server-lib': '1.0.0' } }),
        'tsconfig.json': JSON.stringify({ compilerOptions: OPTIONS, include: ['src'] }),
        'src/db.ts': DB_TS,
        // The entry file: declares the member, and no route file imports it.
        'src/index.ts': [
            "import type { Database } from './db';",
            "declare module 'server-lib' {",
            '  interface ServerInstance { db: Database }',
            '}',
            'export const started = true;',
            '',
        ].join('\n'),
        'src/items.ts': ITEMS_TS,
        'src/wired.ts': ["import './index';", ITEMS_TS].join('\n'),
        // A global augmentation in a declaration file nothing imports.
        'src/globals.d.ts': [
            'export {};',
            'declare global {',
            "  interface Request { tenant: { id: string; plan: 'basic' | 'team' } }",
            '}',
            '',
        ].join('\n'),
        'src/tenant.ts': ['export function tenantOf(request: Request) {', '  return request.tenant;', '}', ''].join('\n'),
    });
    const itemAnchors = (file, text, suffix) => [
        { kind: 'handler_return', alias: `List${suffix}`, symbol_name: 'listItems', source_file: file, anchor_origin: 'llm-symbol' },
        inferAnchor(`Returned${suffix}`, file, text, 'app.db.all()'),
        inferAnchor(`Mapped${suffix}`, file, text, 'rows.map(format)'),
    ];
    it('reads a member a module augmentation declares in a file the handler does not import', () => {
        const root = tree('carrick-2317-plain-', serviceFiles());
        const result = captureStub({
            repoRoot: root,
            serviceName: 'shop',
            outDir: path.join(root, '.carrick', 'stub'),
            anchors: itemAnchors('src/items.ts', ITEMS_TS, ''),
        });
        assert.ok(result.success, result.errors.join('\n'));
        // A handler's return is declared by reference to the handler, so its text names no member.
        assertTyped(result, 'List', [/ReturnType<typeof import\('\.\/src\/\w+'\)\.listItems>/]);
        assertTyped(result, 'Returned', [/\bItem\[\]$/]);
        assertTyped(result, 'Mapped', [/id: string/, /label: string/, /\[\]/]);
    });
    it('reads the same handler the same way when its file does import the augmenting file (control)', () => {
        const root = tree('carrick-2317-control-', serviceFiles());
        const wired = fs.readFileSync(path.join(root, 'src/wired.ts'), 'utf8');
        const result = captureStub({
            repoRoot: root,
            serviceName: 'shop',
            outDir: path.join(root, '.carrick', 'stub'),
            anchors: itemAnchors('src/wired.ts', wired, ''),
        });
        assert.ok(result.success, result.errors.join('\n'));
        // A handler's return is declared by reference to the handler, so its text names no member.
        assertTyped(result, 'List', [/ReturnType<typeof import\('\.\/src\/\w+'\)\.listItems>/]);
        assertTyped(result, 'Returned', [/\bItem\[\]$/]);
        assertTyped(result, 'Mapped', [/id: string/, /label: string/, /\[\]/]);
    });
    it('reads a member a `declare global` declares in a declaration file nothing imports', () => {
        const root = tree('carrick-2317-global-', serviceFiles());
        const tenant = fs.readFileSync(path.join(root, 'src/tenant.ts'), 'utf8');
        const result = captureStub({
            repoRoot: root,
            serviceName: 'shop',
            outDir: path.join(root, '.carrick', 'stub'),
            anchors: [inferAnchor('Tenant', 'src/tenant.ts', tenant, 'request.tenant')],
        });
        assert.ok(result.success, result.errors.join('\n'));
        assertTyped(result, 'Tenant', [/id: string/, /plan: "basic" \| "team"/]);
    });
    it('makes no root of a file that declares no augmentation: the stub is the same with and without it', () => {
        const files = serviceFiles();
        delete files['src/index.ts'];
        delete files['src/wired.ts'];
        delete files['src/globals.d.ts'];
        const capture = (root) => {
            const result = captureStub({
                repoRoot: root,
                serviceName: 'shop',
                outDir: path.join(root, '.carrick', 'stub'),
                anchors: [{ kind: 'symbol', alias: 'Row', symbol_name: 'Item', source_file: 'src/db.ts', anchor_origin: 'llm-symbol' }],
            });
            assert.ok(result.success, result.errors.join('\n'));
            assert.strictEqual(record(result, 'Row').self_check, 'ok');
            const emitted = fs
                .readdirSync(path.join(result.stub_dir, 'types'), { recursive: true })
                .map(String)
                .sort();
            return { surface: surfaceOf(result), emitted };
        };
        const bare = capture(tree('carrick-2317-none-', files));
        // An unimported module that would be emitted, were it a root.
        const withExtra = capture(tree('carrick-2317-none-', { ...files, 'src/unrelated.ts': 'export interface Unrelated { declare: string; module: number }\n' }));
        assert.strictEqual(withExtra.surface, bare.surface);
        assert.deepStrictEqual(withExtra.emitted, bare.emitted);
    });
});
describe('carrick#2317: anchors owned by several projects', () => {
    // The named project owns `app`, and references the project that owns `lib`.
    // Their options are the same, so an anchor keeps its text whichever of them
    // emits the surface; `lib` owns more anchors, so it does.
    const HANDLERS_TS = [
        "import type { ServerInstance } from 'server-lib';",
        'export function fromOwnProject(app: ServerInstance) {',
        '  return app.db.all();',
        '}',
        'export function fromNamedProject(app: ServerInstance) {',
        '  return app.audit.all();',
        '}',
        '',
    ].join('\n');
    const APP_TS = [
        "import type { ServerInstance } from 'server-lib';",
        'export function fromApp(app: ServerInstance) {',
        '  return app.audit.all();',
        '}',
        '',
    ].join('\n');
    const augment = (member) => ["import type { Database } from '../shared/db';", "declare module 'server-lib' {", `  interface ServerInstance { ${member}: Database }`, '}', ''].join('\n');
    it("reads each owner's anchors with that owner's augmentations and no other project's", () => {
        const root = tree('carrick-2317-owners-', {
            ...LIBRARY,
            'package.json': JSON.stringify({ name: 'shop', version: '1.0.0', dependencies: { 'server-lib': '1.0.0' } }),
            'tsconfig.json': JSON.stringify({
                compilerOptions: OPTIONS,
                include: ['app', 'shared'],
                references: [{ path: './tsconfig.lib.json' }],
            }),
            'tsconfig.lib.json': JSON.stringify({ compilerOptions: OPTIONS, include: ['lib', 'shared'] }),
            'shared/db.ts': DB_TS,
            'app/augment.ts': augment('audit'),
            'app/routes.ts': APP_TS,
            'lib/augment.ts': augment('db'),
            'lib/handlers.ts': HANDLERS_TS,
        });
        const result = captureStub({
            repoRoot: root,
            serviceName: 'shop',
            outDir: path.join(root, '.carrick', 'stub'),
            anchors: [
                inferAnchor('Own', 'lib/handlers.ts', HANDLERS_TS, 'app.db.all()'),
                inferAnchor('Foreign', 'lib/handlers.ts', HANDLERS_TS, 'app.audit.all()'),
                inferAnchor('App', 'app/routes.ts', APP_TS, 'app.audit.all()'),
            ],
        });
        assert.ok(result.success, result.errors.join('\n'));
        // The anchors were split by owner: this is the several-owners path.
        assert.ok(result.errors.some((line) => /typed under .*tsconfig\.json, the project that owns their files/.test(line)), JSON.stringify(result.errors));
        // A member a file of the owning project declares, which no anchored file imports.
        assertTyped(result, 'Own', [/\bItem\[\]$/]);
        // The named project's own anchor sees the named project's augmentation.
        assertTyped(result, 'App', [/\bItem\[\]$/]);
        // A member only the OTHER project declares stays as the owner's own
        // compile reads it: that project does not hold the file.
        assertUntyped(result, 'Foreign');
    });
});
describe('carrick#2317: under the untrusted-tree read bound', () => {
    const SENTINEL = 'SENTINEL_2317_outside';
    const saved = process.env[UNTRUSTED_TREE_ENV];
    after(() => {
        if (saved === undefined)
            delete process.env[UNTRUSTED_TREE_ENV];
        else
            process.env[UNTRUSTED_TREE_ENV] = saved;
    });
    const HANDLER_TS = ['export function tenantOf(request: Request) {', '  return request.tenant;', '}', ''].join('\n');
    /** A checkout whose tsconfig lists an augmentation file beside the checkout. */
    const build = () => {
        const base = tree('carrick-2317-bound-', {});
        const checkout = path.join(base, 'checkout');
        const outsideFile = path.join(base, 'outside', 'augment.ts');
        writeInto(base, {
            'outside/augment.ts': [
                'export {};',
                'declare global {',
                `  interface Request { tenant: { id: string; ${SENTINEL}: string } }`,
                '}',
                '',
            ].join('\n'),
        });
        fs.mkdirSync(path.join(checkout, '.git'), { recursive: true });
        writeInto(checkout, {
            'package.json': JSON.stringify({ name: 'shop', version: '1.0.0' }),
            'tsconfig.json': JSON.stringify({ compilerOptions: OPTIONS, include: ['src'], files: [outsideFile] }),
            'src/tenant.ts': HANDLER_TS,
        });
        return { checkout, outsideFile };
    };
    /** One capture, and every path read from disk with `fs.readFileSync` while it ran. */
    const capture = (checkout, name) => {
        const read = new Set();
        const real = fsModule.readFileSync;
        fsModule.readFileSync = ((file, ...rest) => {
            if (typeof file === 'string')
                read.add(path.resolve(file));
            return real(file, ...rest);
        });
        syncBuiltinESMExports();
        try {
            const result = captureStub({
                repoRoot: checkout,
                serviceName: name,
                outDir: path.join(path.dirname(checkout), `stub-${name}`),
                anchors: [inferAnchor('Tenant', 'src/tenant.ts', HANDLER_TS, 'request.tenant')],
            });
            return { result, read };
        }
        finally {
            fsModule.readFileSync = real;
            syncBuiltinESMExports();
        }
    };
    it('switch unset: the outside augmentation is read and the member resolves (the known positive)', () => {
        delete process.env[UNTRUSTED_TREE_ENV];
        const { checkout, outsideFile } = build();
        const { result, read } = capture(checkout, 'open');
        assert.ok(result.success, result.errors.join('\n'));
        assert.match(aliasText(result, 'Tenant'), new RegExp(SENTINEL));
        assert.ok(read.has(outsideFile), 'the recorder sees the read it is there to refuse');
    });
    it('switch set: the outside augmentation is not read, and the member stays unresolved', () => {
        process.env[UNTRUSTED_TREE_ENV] = '1';
        const { checkout, outsideFile } = build();
        const { result, read } = capture(checkout, 'bound');
        assert.ok(result.success, result.errors.join('\n'));
        assert.ok(!read.has(outsideFile), 'the file outside the checkout was read');
        assert.ok(!(JSON.stringify(result) + surfaceOf(result)).includes(SENTINEL), 'the outside declaration reached the capture');
        assertUntyped(result, 'Tenant');
    });
});
