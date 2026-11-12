/**
 * carrick#2418: a workspace package whose output is not built is typed from
 * its source under its own project's options, as an editor types it.
 *
 * A service config that references other projects tells the compiler two
 * things an editor acts on:
 *
 *  - Rule A, the owner: a project does not own a file that one of the
 *    projects it references lists. tsserver's default-project rule.
 *  - Rule B, the references: in an owner's program, an import that lands on
 *    a referenced project's declaration output (`dist/index.d.ts`, never
 *    built) reads that project's source instead, and an import made from a
 *    referenced project's own file resolves under that project's options
 *    (its `paths`, its conditions).
 *
 * The fixture (`test/fixtures/reference-redirect/`) is materialised in a temp
 * dir with the links a workspace install writes. Its service config lists
 * every file by default include and references only `tsconfig.src.json`,
 * which references three packages. A fourth package resolves only through
 * the source project's custom export condition (Rule A alone), and a fifth
 * is never referenced, so it stays unresolved, as in the editor.
 *
 * The oracle is the compiler's own editor mode: `ts.createProgram` handed the
 * project references with `useSourceOfProjectReferenceRedirect`. The
 * loader's error-type positions on the fixture are exactly that program's.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
import { captureStub, referenceRedirectOf, serviceConfigPath, sidecarCompilerOptions } from '../src/capture/index.js';
import { redirectingCompilerHost } from '../src/capture/reference-redirect.js';
import { FLAG_ERROR_TYPE, kindOf } from '../src/position-rows.js';
import { SidecarClient } from './helpers.js';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'reference-redirect');
/** The sidecar's own `@types/node`, linked in so `node:path` has types. */
const TYPES_NODE = fs.realpathSync.native(path.join(__dirname, '..', '..', 'node_modules', '@types', 'node'));
const PACKAGES = ['shapes', 'events', 'legacy', 'cjs', 'orphan'];
const HANDLER = 'services/api/src/handler.ts';
const roots = [];
after(() => {
    for (const root of roots)
        fs.rmSync(root, { recursive: true, force: true });
});
/**
 * The fixture in a temp dir, as a checkout with its workspace links, and the
 * service config replaced by `serviceConfig` when given.
 */
function materialise(serviceConfig) {
    const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2418-')));
    roots.push(root);
    const repo = path.join(root, 'repo');
    fs.cpSync(FIXTURE, repo, { recursive: true });
    fs.mkdirSync(path.join(repo, '.git'));
    fs.mkdirSync(path.join(repo, 'node_modules', '@acme'), { recursive: true });
    for (const name of PACKAGES) {
        fs.symlinkSync(path.join('..', '..', 'packages', name), path.join(repo, 'node_modules', '@acme', name), 'dir');
    }
    fs.mkdirSync(path.join(repo, 'node_modules', '@types'));
    fs.symlinkSync(TYPES_NODE, path.join(repo, 'node_modules', '@types', 'node'), 'dir');
    const service = path.join(repo, 'services', 'api');
    if (serviceConfig)
        fs.writeFileSync(path.join(service, 'tsconfig.json'), JSON.stringify(serviceConfig));
    return { root, repo, service, handler: path.join(repo, HANDLER), store: path.join(root, 'store') };
}
const SOURCE = fs.readFileSync(path.join(FIXTURE, HANDLER), 'utf8');
function lineOf(text) {
    const at = SOURCE.indexOf(text);
    assert.ok(at >= 0, `the fixture must contain: ${text}`);
    return SOURCE.slice(0, at).split('\n').length;
}
/** The span of the `nth` occurrence of `text` in the handler. */
function spanOf(text, nth = 0) {
    let at = -1;
    for (let i = 0; i <= nth; i += 1)
        at = SOURCE.indexOf(text, at + 1);
    assert.ok(at >= 0, `the fixture must contain: ${text}`);
    return [at, at + text.length];
}
/** `function_return` for the handler's two functions, from a fresh sidecar. */
async function inferReturns(ws) {
    const client = new SidecarClient();
    await client.start();
    try {
        await client.send({ action: 'init', request_id: 'init', repo_root: ws.service, tsconfig_path: 'tsconfig.json' });
        const res = await client.send({
            action: 'infer',
            request_id: 'infer',
            requests: [
                {
                    file_path: ws.handler,
                    line_number: lineOf('export async function summary('),
                    infer_kind: 'function_return',
                    alias: 'Summary_Return',
                },
                {
                    file_path: ws.handler,
                    line_number: lineOf('export function stray('),
                    infer_kind: 'function_return',
                    alias: 'Stray_Return',
                },
            ],
        }, 60_000);
        return new Map((res.inferred_types ?? []).map((row) => [row.alias, row]));
    }
    finally {
        await client.stop();
    }
}
/** The walk's rows for the handler, from a fresh sidecar. */
async function handlerRows(ws) {
    const client = new SidecarClient();
    await client.start();
    try {
        await client.send({
            action: 'init',
            request_id: 'init',
            repo_root: ws.service,
            tsconfig_path: 'tsconfig.json',
            scan_root: ws.repo,
        });
        const outPath = path.join(ws.store, 'positions', 'api.jsonl.gz');
        const answer = await client.send({
            action: 'position_rows',
            request_id: 'rows',
            files: [ws.handler],
            scan_root: ws.repo,
            out_path: outPath,
            service: 'api',
        }, 60_000);
        assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
        const lines = zlib
            .gunzipSync(fs.readFileSync(outPath))
            .toString('utf8')
            .split('\n')
            .filter((line) => line.trim() !== '')
            .map((line) => JSON.parse(line));
        const file = lines.find((line) => line.f === HANDLER);
        assert.ok(file, `no rows for the handler: ${lines.map((l) => l.f).join(', ')}`);
        return file.r;
    }
    finally {
        await client.stop();
    }
}
function rowAt(rows, [start, end], kind = 'member') {
    const row = rows.find((r) => r[0] === kind && r[1] === start && r[2] === end);
    assert.ok(row, `no ${kind} row at ${start}-${end} (${SOURCE.slice(start, end)})`);
    return row;
}
const hasError = (row) => (row[8] & FLAG_ERROR_TYPE) !== 0;
/** Every file under `dir`, relative, leaving out `.carrick/`. */
function treeOf(dir) {
    const out = [];
    const walk = (at) => {
        for (const entry of fs.readdirSync(at, { withFileTypes: true })) {
            if (entry.name === '.carrick')
                continue;
            const abs = path.join(at, entry.name);
            if (entry.isDirectory())
                walk(abs);
            else
                out.push(path.relative(dir, abs));
        }
    };
    walk(dir);
    return out.sort();
}
function captureHandler(ws) {
    const result = captureStub({
        repoRoot: ws.service,
        serviceName: 'api',
        outDir: path.join(ws.store, 'stub'),
        tsconfigPath: 'tsconfig.json',
        anchors: [
            {
                kind: 'symbol',
                alias: 'Endpoint_placed_Response',
                symbol_name: 'Placed',
                source_file: 'src/handler.ts',
                anchor_origin: 'llm-symbol',
            },
            {
                kind: 'symbol',
                alias: 'Endpoint_lost_Response',
                symbol_name: 'Lost',
                source_file: 'src/handler.ts',
                anchor_origin: 'llm-symbol',
            },
            {
                kind: 'symbol',
                alias: 'Endpoint_priced_Response',
                symbol_name: 'Priced',
                source_file: 'src/handler.ts',
                anchor_origin: 'llm-symbol',
            },
        ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    return new Map(result.aliases.map((record) => [record.alias, record]));
}
const unresolvedImports = (record) => (record?.any_provenance ?? []).filter((p) => p.reason === 'unresolved_import').map((p) => p.path);
describe('carrick#2418: referenced packages are typed from source, as an editor types them', () => {
    let ws;
    let inferred;
    let rows;
    let records;
    let before7;
    let after7;
    before(async () => {
        ws = materialise();
        before7 = treeOf(ws.repo);
        inferred = await inferReturns(ws);
        rows = await handlerRows(ws);
        records = captureHandler(ws);
        after7 = treeOf(ws.repo);
    });
    it('1. owner: the project the service config references owns the file it also lists', () => {
        const configFor = serviceConfigPath(path.join(ws.service, 'tsconfig.json'));
        assert.strictEqual(configFor(ws.handler), path.join(ws.service, 'tsconfig.src.json'));
    });
    it('2. infer: the body names every referenced and conditioned package type, with no any', () => {
        const body = inferred.get('Summary_Return');
        assert.ok(body, `no inference: ${JSON.stringify([...inferred.values()])}`);
        for (const member of [
            'id: string',
            'amount: number',
            'currency: string',
            'base: string',
            'orderId: string',
            'placedAt: number',
            'memo: string',
            'closed: boolean',
            'reference: string',
        ]) {
            assert.ok(body.type_string.includes(member), `${member} missing from ${body.type_string}`);
        }
        assert.ok(!/\bany\b/.test(body.type_string), body.type_string);
        assert.strictEqual(body.any_provenance, undefined, JSON.stringify(body.any_provenance));
    });
    it('2. infer: a package the service does not reference stays the error type', () => {
        const lost = inferred.get('Stray_Return');
        assert.ok(lost);
        assert.strictEqual(lost.type_string, 'Stray');
        assert.ok(lost.any_provenance, 'an unresolved return carries its provenance');
    });
    it('3. rows: the uses of the four imports carry no error type and name the sibling source', () => {
        for (const use of ['order.id', 'placed.orderId', 'ledger.closed', 'receipt.reference']) {
            const row = rowAt(rows, spanOf(use));
            assert.ok(!hasError(row), `${use}: ${JSON.stringify(row)}`);
        }
        assert.strictEqual(rowAt(rows, spanOf('order.id'))[4], 'packages/shapes/src/index.ts');
        assert.strictEqual(rowAt(rows, spanOf('placed.orderId'))[4], 'packages/events/src/index.ts');
        assert.strictEqual(rowAt(rows, spanOf('ledger.closed'))[4], 'packages/legacy/src/index.ts');
        assert.strictEqual(rowAt(rows, spanOf('receipt.reference'))[4], 'packages/cjs/src/index.cts');
        assert.strictEqual(rowAt(rows, spanOf('ledger.entries[0].memo'))[4], 'packages/legacy/src/entry.ts');
    });
    it('3. rows: the use of the unreferenced package keeps the error type', () => {
        assert.ok(hasError(rowAt(rows, spanOf('value.note'))));
    });
    it("4. a referenced package's own paths alias resolves under its own options only", () => {
        const row = rowAt(rows, spanOf('order.total.amount'));
        assert.ok(!hasError(row), JSON.stringify(row));
        assert.strictEqual(row[4], 'packages/shapes/src/money.ts');
        // The service's options carry no such alias.
        const serviceOptions = ts.getParsedCommandLineOfConfigFile(path.join(ws.service, 'tsconfig.src.json'), {}, { ...ts.sys, onUnRecoverableConfigFileDiagnostic: () => { } }).options;
        const fromShapes = path.join(ws.repo, 'packages', 'shapes', 'src', 'index.ts');
        assert.strictEqual(ts.resolveModuleName('@shapes-internal/money', fromShapes, serviceOptions, ts.sys).resolvedModule, undefined);
    });
    it('5. capture: a symbol anchor naming the referenced types never reads as declared', () => {
        // The anchor program resolves Order; the surface emit, which keeps the
        // compiler's own resolution, still reaches the unbuilt output, so the
        // emitted text may hold `unknown` there. Its cause is that import, never
        // an author's `unknown`: each position is either typed or labelled with it.
        const placed = records.get('Endpoint_placed_Response');
        assert.ok(placed, 'no record for the anchor');
        const declared = (placed.any_provenance ?? []).filter((p) => p.reason === 'declared');
        assert.deepStrictEqual(declared, [], JSON.stringify(placed));
        // A member reached through the package's own paths alias.
        const priced = records.get('Endpoint_priced_Response');
        assert.ok(priced, 'no record for the priced anchor');
        assert.deepStrictEqual(unresolvedImports(priced), [], JSON.stringify(priced));
        // The known positive: the unreferenced package still reads unresolved.
        assert.ok(unresolvedImports(records.get('Endpoint_lost_Response')).length > 0, JSON.stringify(records.get('Endpoint_lost_Response')));
    });
    it("5. capture: a sibling's source inside the checkout is the user's code, not a library", () => {
        // The anchor program as the capture builds it. A source reached through a
        // node_modules link would otherwise be marked an external library, and
        // every reader that skips libraries (the walk for an unresolved import's
        // cause, the emit) would skip the user's own code.
        const configPath = path.join(ws.service, 'tsconfig.src.json');
        const parsed = ts.getParsedCommandLineOfConfigFile(configPath, {}, {
            ...ts.sys,
            onUnRecoverableConfigFileDiagnostic: () => { },
        });
        const redirect = referenceRedirectOf(configPath, ws.service);
        assert.ok(redirect, 'the source project references packages');
        const options = sidecarCompilerOptions(parsed.options);
        const program = ts.createProgram(parsed.fileNames, options, redirectingCompilerHost(ts.createCompilerHost(options), options, redirect));
        const shapes = program.getSourceFile(path.join(ws.repo, 'packages', 'shapes', 'src', 'index.ts'));
        assert.ok(shapes, 'the sibling source is in the program');
        assert.strictEqual(program.isSourceFileFromExternalLibrary(shapes), false);
        // Reached through shapes' own paths alias: the import resolved under its options.
        assert.ok(program.getSourceFile(path.join(ws.repo, 'packages', 'shapes', 'src', 'money.ts')));
    });
    it('7. nothing is written into the tree by infer, the walk or a capture', () => {
        assert.deepStrictEqual(after7, before7);
    });
    it('8. a referenced project leaving types unset still reads a node builtin', () => {
        const row = rowAt(rows, spanOf('order.file.base'));
        assert.ok(!hasError(row), JSON.stringify(row));
        assert.strictEqual(row[7], 'string');
    });
    it("8. a referenced project's own imports resolve under its TypeScript 5 defaults", () => {
        // legacy sets module commonjs and leaves moduleResolution and
        // resolveJsonModule unset. Its own TypeScript 5 compiler reads that as
        // node10 without JSON modules, so its JSON import does not resolve;
        // TypeScript 6's defaults (bundler, JSON on) would resolve it. The
        // sidecar reads every project as TypeScript 5 does (carrick#2019).
        const row = rowAt(rows, spanOf('ledger.defaults.rounding'));
        assert.ok(hasError(row), JSON.stringify(row));
        assert.ok(!hasError(rowAt(rows, spanOf('ledger.closed'))));
    });
    it("10. the loader's error-type positions are the editor's, but for TypeScript 5's layout", () => {
        // The one named difference: TypeScript 6's editor lays out a project that
        // leaves rootDir unset under its config's directory (dist/src/index.d.ts),
        // so it misses the legacy package's dist/index.d.ts and types every use
        // of it as the error type. The loader lays it out as TypeScript 5 does.
        const loader = errorSpans(rows);
        const editor = editorErrorSpans(ws);
        const legacyUse = (span) => {
            const [start, end] = span.split(':')[1].split('-').map(Number);
            return SOURCE.slice(start, end).startsWith('ledger');
        };
        const editorOnly = editor.filter((span) => !loader.includes(span));
        assert.deepStrictEqual(loader.filter((span) => !editor.includes(span)), [], 'error types the editor does not have');
        assert.deepStrictEqual(editorOnly.filter((span) => !legacyUse(span)), [], 'error types the loader misses');
        assert.ok(editorOnly.length > 0, 'the editor no longer differs on the legacy layout: drop the exception');
    });
});
describe("carrick#2418: Rule A's one flip", () => {
    it('9. a named config listing src itself yields src to the project it references', async () => {
        const ws = materialise({ include: ['src'], references: [{ path: './tsconfig.src.json' }] });
        const configFor = serviceConfigPath(path.join(ws.service, 'tsconfig.json'));
        assert.strictEqual(configFor(ws.handler), path.join(ws.service, 'tsconfig.src.json'));
        const body = (await inferReturns(ws)).get('Summary_Return');
        assert.ok(body?.type_string.includes('placedAt: number'), body?.type_string);
    });
});
/** `kind:start-end` of every row carrying the error type. */
function errorSpans(rows) {
    return rows.filter(hasError).map((r) => `${r[0]}:${r[1]}-${r[2]}`).sort();
}
/**
 * The same spans from the compiler's editor mode: the owner's program built
 * with its project references and `useSourceOfProjectReferenceRedirect`.
 */
function editorErrorSpans(ws) {
    const parse = (configPath) => {
        const parsed = ts.getParsedCommandLineOfConfigFile(configPath, {}, {
            ...ts.sys,
            onUnRecoverableConfigFileDiagnostic: () => { },
        });
        const options = sidecarCompilerOptions(parsed.options);
        Object.defineProperty(options, 'configFile', { value: parsed.options.configFile, enumerable: false });
        return { ...parsed, options };
    };
    const owner = parse(path.join(ws.service, 'tsconfig.src.json'));
    const host = ts.createCompilerHost(owner.options);
    host.useSourceOfProjectReferenceRedirect = () => true;
    host.getParsedCommandLine = (file) => parse(file);
    const program = ts.createProgram({
        rootNames: owner.fileNames,
        options: owner.options,
        projectReferences: owner.projectReferences,
        host,
    });
    const checker = program.getTypeChecker();
    const source = program.getSourceFile(ws.handler);
    const spans = [];
    const visit = (node) => {
        const kind = kindOf(node);
        if (kind) {
            const typed = kind === 'compare' ? node.left : node;
            const type = checker.getTypeAtLocation(typed);
            if (type.intrinsicName === 'error') {
                spans.push(`${kind}:${node.getStart(source)}-${node.getEnd()}`);
            }
        }
        ts.forEachChild(node, visit);
    };
    ts.forEachChild(source, visit);
    return spans.sort();
}
