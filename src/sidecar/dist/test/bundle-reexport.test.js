/**
 * carrick#1605: a symbol the model names through a barrel failed the bundle's
 * symbol check.
 *
 * The model reports a type where the consumer imports it from, and a consumer
 * usually imports from a package's index: `import type { Me } from
 * "./core/index.js"`, where the index only has `export * from ...`. The bundle
 * looked for a declaration of that name in the named file itself, so every
 * such row logged `Symbol 'Me' not found` and lost its explicit anchor.
 *
 * The name is now followed the way an import of it is: through `export *`
 * (any depth), `export { X } from` and `export { X as Y } from`, to the
 * declaration it resolves to. A name two `export *` sources both provide is
 * ambiguous (TypeScript reports TS2308 and an ES module exports neither), so
 * it stays a symbol failure rather than a silent pick of the first.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const FILES = {
    'tsconfig.json': JSON.stringify({
        compilerOptions: {
            target: 'ES2022',
            module: 'NodeNext',
            moduleResolution: 'NodeNext',
            strict: true,
            skipLibCheck: true,
        },
        include: ['src'],
    }),
    'src/core/api.ts': [
        'export type Me = { id: string; email: string };',
        'export interface Account { id: string; plan: "basic" | "pro" }',
        'export type Status = "active" | "suspended";',
        '',
    ].join('\n'),
    // Two levels: the index re-exports an inner barrel that re-exports the file.
    'src/core/inner/index.ts': 'export * from "../api.js";\n',
    'src/core/index.ts': [
        'export * from "./inner/index.js";',
        'export { Account as AccountView } from "./api.js";',
        '',
    ].join('\n'),
    'src/named/index.ts': 'export { Me } from "../core/api.js";\n',
    'src/clash/a.ts': 'export type Clash = { a: number };\n',
    'src/clash/b.ts': 'export type Clash = { b: string };\n',
    'src/clash/index.ts': 'export * from "./a.js";\nexport * from "./b.js";\n',
    // An installed package re-exported by the service: its declarations name
    // types from the package's own files, which the bundle does not carry.
    'node_modules/extpkg/package.json': JSON.stringify({ name: 'extpkg', version: '1.0.0', types: './index.d.ts' }),
    'node_modules/extpkg/inner.d.ts': 'export interface Inner { deep: number }\n',
    'node_modules/extpkg/index.d.ts': "import type { Inner } from './inner';\nexport interface Ext { inner: Inner; tag: 'ext' }\n",
    'src/ext/index.ts': "export * from 'extpkg';\n",
    'src/ext/named.ts': "export { Ext as Renamed } from 'extpkg';\n",
    'src/client.ts': [
        'import type { Me } from "./core/index.js";',
        '',
        'export async function me(): Promise<Me> {',
        '  const res = await fetch("/api/v1/me");',
        '  return (await res.json()) as Me;',
        '}',
        '',
    ].join('\n'),
};
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick#1605: the bundle follows a name through its re-exports', () => {
    let client;
    let repo;
    before(async () => {
        repo = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1605-'));
        for (const [rel, text] of Object.entries(FILES)) {
            const abs = path.join(repo, rel);
            fs.mkdirSync(path.dirname(abs), { recursive: true });
            fs.writeFileSync(abs, text);
        }
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repo });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repo, { recursive: true, force: true });
    });
    async function bundle(symbol_name, source_file, extra = {}) {
        return client.send({
            action: 'bundle',
            request_id: `${symbol_name}@${source_file}`,
            symbols: [{ symbol_name, source_file, ...extra }],
        }, 30000);
    }
    it('resolves a type through `export *` two levels deep', async () => {
        const res = await bundle('Me', 'src/core/index.ts', { alias: 'Me_Barrel' });
        assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
        assert.match(collapse(res.dts_content ?? ''), /export type Me_Barrel = \{ id: string; email: string; \};/);
    });
    it('resolves a type through `export { Me } from`', async () => {
        const res = await bundle('Me', 'src/named/index.ts', { alias: 'Me_Named' });
        assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
        assert.match(collapse(res.dts_content ?? ''), /export type Me_Named = \{ id: string; email: string; \};/);
    });
    it('declares a renamed re-export under the name it is exported as', async () => {
        const res = await bundle('AccountView', 'src/core/index.ts');
        assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
        const dts = collapse(res.dts_content ?? '');
        assert.match(dts, /export interface AccountView \{ id: string; plan: "basic" \| "pro"; \}/);
        assert.doesNotMatch(dts, /\bAccount\b(?!View)/);
    });
    it('wraps a re-exported union in parentheses before an array suffix', async () => {
        const res = await bundle('Status', 'src/core/index.ts', { alias: 'Statuses', array_depth: 1 });
        assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
        assert.match(collapse(res.dts_content ?? ''), /export type Statuses = \("active" \| "suspended"\)\[\];/);
    });
    it('keeps a name two `export *` sources both provide as a failure, not a pick', async () => {
        const res = await bundle('Clash', 'src/clash/index.ts');
        assert.strictEqual(res.dts_content, undefined);
        const failure = res.symbol_failures?.find((f) => f.symbol_name === 'Clash');
        assert.ok(failure, `expected a symbol failure: ${JSON.stringify(res)}`);
        assert.match(failure.reason, /more than one/);
    });
    it('keeps a name re-exported from an installed package a failure, as before', async () => {
        // Following it would print the package's declaration with its own
        // imports dangling (`inner: Inner`); the failure lets inference keep the row.
        for (const [symbol, file] of [
            ['Ext', 'src/ext/index.ts'],
            ['Renamed', 'src/ext/named.ts'],
        ]) {
            const res = await bundle(symbol, file);
            assert.strictEqual(res.dts_content, undefined, `${symbol}: ${res.dts_content}`);
            const failure = res.symbol_failures?.find((f) => f.symbol_name === symbol);
            assert.ok(failure, `expected a symbol failure for ${symbol}: ${JSON.stringify(res)}`);
            assert.match(failure.reason, /installed package/);
        }
    });
    it('still fails a name the barrel does not export', async () => {
        const res = await bundle('Nope', 'src/core/index.ts');
        const failure = res.symbol_failures?.find((f) => f.symbol_name === 'Nope');
        assert.ok(failure, `expected a symbol failure: ${JSON.stringify(res)}`);
        assert.match(failure.reason, /not found/);
    });
});
/**
 * carrick#2304: a package of the checkout that a package manager links into a
 * service's `node_modules` is the user's own source. The compiler marks every
 * file it reaches through a `node_modules` lookup as an external library,
 * whatever the file's real path, so that flag alone read the sibling package
 * as installed and every type named through it lost its explicit anchor.
 */
const LINKED_FILES = {
    'packages/shapes/package.json': JSON.stringify({
        name: '@depot/shapes',
        type: 'module',
        types: './src/index.ts',
        exports: { '.': { types: './src/index.ts', default: './src/index.ts' } },
    }),
    'packages/shapes/src/index.ts': "export * from './crate.js';\nexport type { Pallet } from './pallet.js';\n",
    'packages/shapes/src/crate.ts': 'export interface Crate { id: string; weightKg: number; fragile: boolean | null }\n',
    'packages/shapes/src/pallet.ts': "import type { Crate } from './crate.js';\nexport interface Pallet { id: string; crates: Crate[] }\n",
    'packages/web/package.json': JSON.stringify({ name: '@depot/web', type: 'module' }),
    'packages/web/tsconfig.json': JSON.stringify({
        compilerOptions: {
            target: 'ES2022',
            module: 'NodeNext',
            moduleResolution: 'NodeNext',
            strict: true,
            skipLibCheck: true,
        },
        include: ['src'],
    }),
    // Installed: a real directory under `node_modules`, not a package of the checkout.
    'packages/web/node_modules/shelf-lib/package.json': JSON.stringify({
        name: 'shelf-lib',
        version: '1.0.0',
        types: './index.d.ts',
    }),
    'packages/web/node_modules/shelf-lib/index.d.ts': 'export interface Shelf { id: string; slots: number }\n',
    'packages/web/src/api.ts': "import type { Crate } from '@depot/shapes';\nexport type { Crate } from '@depot/shapes';\nexport type First = Crate;\n",
    'packages/web/src/reexp.ts': "export type { Shelf } from 'shelf-lib';\n",
};
describe('carrick#2304: a package of the checkout linked through node_modules is followed', () => {
    let client;
    let repo;
    before(async () => {
        // The real path: the compiler names a linked package's files by it.
        repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2304-')));
        for (const [rel, text] of Object.entries(LINKED_FILES)) {
            const abs = path.join(repo, rel);
            fs.mkdirSync(path.dirname(abs), { recursive: true });
            fs.writeFileSync(abs, text);
        }
        // The checkout is the nearest `.git` above the service root.
        fs.mkdirSync(path.join(repo, '.git'));
        const scope = path.join(repo, 'packages/web/node_modules/@depot');
        fs.mkdirSync(scope, { recursive: true });
        fs.symlinkSync('../../../shapes', path.join(scope, 'shapes'), 'dir');
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: path.join(repo, 'packages/web') });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repo, { recursive: true, force: true });
    });
    async function bundle(symbol_name, source_file, alias) {
        return client.send({
            action: 'bundle',
            request_id: alias,
            symbols: [{ symbol_name, source_file, alias }],
        }, 30000);
    }
    const CRATE = '{ id: string; weightKg: number; fragile: boolean | null; }';
    const PALLET = `{ id: string; crates: ${CRATE}[]; }`;
    /** The bundle declares `alias` with exactly `body`, and reports no failure. */
    function assertDeclares(res, alias, body) {
        assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
        const dts = collapse(res.dts_content ?? '');
        assert.ok(dts.includes(`export interface ${alias} ${body}`) || dts.includes(`export type ${alias} = ${body};`), `expected ${alias} to be ${body}, got: ${dts}`);
    }
    it('resolves a type named at the linked package index, through `export *` and `export { X } from`', async () => {
        const index = path.join(repo, 'packages/shapes/src/index.ts');
        assertDeclares(await bundle('Crate', index, 'Crate_Index'), 'Crate_Index', CRATE);
        assertDeclares(await bundle('Pallet', index, 'Pallet_Index'), 'Pallet_Index', PALLET);
    });
    it('resolves the same type named at a service file that re-exports the package', async () => {
        assertDeclares(await bundle('Crate', 'src/api.ts', 'Crate_Service'), 'Crate_Service', CRATE);
    });
    it('keeps a package installed as a real directory under node_modules a failure, with the same reason', async () => {
        const res = await bundle('Shelf', 'src/reexp.ts', 'Shelf_Service');
        assert.strictEqual(res.dts_content, undefined, `${res.dts_content}`);
        const failure = res.symbol_failures?.find((f) => f.symbol_name === 'Shelf');
        assert.ok(failure, `expected a symbol failure: ${JSON.stringify(res)}`);
        assert.strictEqual(failure.reason, "Symbol 'Shelf' is re-exported from an installed package in src/reexp.ts");
    });
});
