/**
 * The `implementations` action (carrick#2432 slice B): where discovery stops
 * at a call through a bodyless declaration, the analysis process names the
 * declaration's implementations.
 *
 * The fixture (`test/fixtures/implementations/`) is an interface `Port` with
 * two overloaded `send` signatures, implemented by a class, by a class whose
 * method is decorated, and by an object literal; a handler table whose
 * entries call `send` on an interface-typed parameter; and direct calls to
 * each implementation. The test adds an interface with more implementations
 * than the cap.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'implementations', 'repo');
/** Seventeen classes implementing one interface: one past the cap. */
const MANY = [
    'export interface Step {',
    '  run(): void;',
    '}',
    ...Array.from({ length: 17 }, (_, i) => `export class Step${i} implements Step { run(): void {} }`),
    'export function go(step: Step): void {',
    '  step.run();',
    '}',
    '',
].join('\n');
describe('implementations: go-to-implementation where discovery stops (carrick#2432)', () => {
    let root;
    let repo;
    let answers;
    let rows;
    /** The span of the first occurrence of `text` in `file`. */
    const span = (file, text) => {
        const source = fs.readFileSync(path.join(repo, file), 'utf8');
        const start = source.indexOf(text);
        assert.ok(start >= 0, `${text} is in ${file}`);
        return { file: path.join(repo, file), start, end: start + text.length };
    };
    const QUERIES = [
        ['src/handlers.ts', "port.send('created', { id: 1 })"],
        ['src/handlers.ts', "port.send('deleted')"],
        ['src/many.ts', 'step.run()'],
        ['src/direct.ts', "new HttpPort().send('a')"],
        ['src/direct.ts', 'export async function direct'],
        ['src/port.ts', 'send'],
    ];
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-implementations-')));
        repo = path.join(root, 'trees', 'checkout');
        fs.mkdirSync(path.join(root, 'store'));
        fs.cpSync(FIXTURE, repo, { recursive: true });
        fs.writeFileSync(path.join(repo, 'src', 'many.ts'), MANY);
        const client = new SidecarClient();
        await client.start();
        try {
            const ready = await client.send({
                action: 'init',
                request_id: 'init',
                repo_root: repo,
                tsconfig_path: 'tsconfig.json',
                scan_root: repo,
            });
            assert.strictEqual(ready.status, 'ready');
            const answer = await client.send({
                action: 'implementations',
                request_id: 'impl',
                queries: QUERIES.map(([file, text]) => span(file, text)),
                scan_root: repo,
            }, 60_000);
            assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
            answers = answer.result?.answers ?? [];
            // The rows of the direct calls, from the same process afterwards: what
            // an implementation must be named as to join to its definition.
            const out = path.join(root, 'store', 'rows.jsonl.gz');
            const walked = await client.send({ action: 'position_rows', request_id: 'rows', files: [path.join(repo, 'src', 'direct.ts')], scan_root: repo, out_path: out, service: 'svc' }, 60_000);
            assert.strictEqual(walked.status, 'success', JSON.stringify(walked.errors));
            rows = new Map();
            for (const line of zlib.gunzipSync(fs.readFileSync(out)).toString('utf8').trimEnd().split('\n').slice(1)) {
                const file = JSON.parse(line);
                rows.set(file.f, file.r);
            }
        }
        finally {
            await client.stop();
        }
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('answers once per question, in request order', () => {
        assert.strictEqual(answers.length, QUERIES.length);
    });
    it('names the interface method a handler-table call goes through, and its three implementations in file order, without its own overloads', () => {
        const [created] = answers;
        assert.deepStrictEqual(created.declaration, {
            symbol: 'send',
            decl_file: 'src/port.ts',
            decl_line: 2,
            decl_package: 'svc',
        });
        assert.strictEqual(created.too_many, false);
        assert.strictEqual(created.reason, undefined);
        assert.deepStrictEqual(created.implementations.map((i) => [i.symbol, i.decl_file]), [
            ['send', 'src/http-port.ts'],
            ['send', 'src/memory-port.ts'],
            ['send', 'src/queue-port.ts'],
        ]);
        assert.ok(created.implementations.every((i) => i.decl_file !== 'src/port.ts'), 'the second signature of the interface method is its own, not an implementation');
    });
    it('names each implementation as a call row at it does, decorated method included', () => {
        const source = fs.readFileSync(path.join(repo, 'src', 'direct.ts'), 'utf8');
        const callRow = (text) => {
            const start = source.indexOf(text);
            const row = (rows.get('src/direct.ts') ?? []).find((r) => r[0] === 'call' && r[1] === start && r[2] === start + text.length);
            assert.ok(row, `a call row at ${text}`);
            return { symbol: row[3], decl_file: row[4], decl_line: row[5], decl_package: row[6] };
        };
        assert.deepStrictEqual(answers[0].implementations, [
            callRow("new HttpPort().send('a')"),
            callRow("memoryPort.send('c')"),
            callRow("new QueuePort().send('b')"),
        ]);
        // The decorator's line, where the declaration starts, not the name's.
        assert.strictEqual(answers[0].implementations[2].decl_line, 8);
    });
    it('gives a second call through the same declaration the same answer', () => {
        assert.deepStrictEqual(answers[1], answers[0]);
    });
    it('names none past the cap, and says so', () => {
        assert.deepStrictEqual(answers[2], {
            declaration: { symbol: 'run', decl_file: 'src/many.ts', decl_line: 2, decl_package: 'svc' },
            implementations: [],
            too_many: true,
        });
    });
    it('asks nothing for a declaration with a body', () => {
        assert.strictEqual(answers[3].reason, 'has_body');
        assert.deepStrictEqual(answers[3].implementations, []);
        assert.strictEqual(answers[3].declaration?.decl_file, 'src/http-port.ts');
    });
    it('answers a span that is no call or member with no_call', () => {
        assert.deepStrictEqual(answers[4], { declaration: null, implementations: [], too_many: false, reason: 'no_call' });
        assert.strictEqual(answers[5].reason, 'no_call');
    });
});
