/**
 * carrick#3509: a default-library member is named at its own declaration
 * line, on both sides the scanner joins: the position rows of a service's
 * calls, and the declarations a verified op on a `lib:` global names.
 *
 * ts-morph's in-memory copy of `lib.*.d.ts` is minified, so every member of
 * `lib.dom.d.ts` sat on its line 3: `response.json()` and
 * `crypto.randomUUID()` named the same declaration as
 * `BroadcastChannel.addEventListener`, and took that member's verified op.
 * Each line asserted here is a line of the compiler's own `lib.dom.d.ts` that
 * declares the member.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import ts from 'typescript';
import { ts as tsMorphTs } from 'ts-morph';
import { defaultLibraryTexts } from '../src/default-library.js';
import { SidecarClient } from './helpers.js';
const DOM = 'lib:lib.dom.d.ts';
const DOM_LINES = fs
    .readFileSync(path.join(path.dirname(ts.getDefaultLibFilePath({})), 'lib.dom.d.ts'), 'utf8')
    .split('\n');
const SOURCE = `export async function load(response: Response): Promise<unknown> {
  return response.json();
}

export function newId(): string {
  return crypto.randomUUID();
}

export function listen(onMessage: (event: MessageEvent) => void): void {
  new BroadcastChannel('updates').addEventListener('message', onMessage);
}
`;
/** Asserts `line` (1-based) of the compiler's `lib.dom.d.ts` declares `member`. */
function declares(line, member) {
    assert.ok(typeof line === 'number' && line > 3, `${member} at line ${line}`);
    assert.match(DOM_LINES[line - 1], new RegExp(`\\b${member}[?]?[<(:]`), `line ${line}: ${DOM_LINES[line - 1]}`);
}
describe('the default library the projects read (carrick#3509)', () => {
    it('comes from the compiler ts-morph checks with, and holds every file a program can name', () => {
        // The texts are the `typescript` package's; the checker is ts-morph's own copy.
        assert.strictEqual(ts.version, tsMorphTs.version);
        // `lib.d.ts` (the ES5 default an empty tsconfig loads) has no middle segment.
        for (const name of ['lib.d.ts', 'lib.es5.d.ts', 'lib.dom.d.ts', 'lib.esnext.full.d.ts']) {
            assert.ok(defaultLibraryTexts().has(name), name);
        }
    });
});
describe('default-library members keep their own declaration lines (carrick#3509)', () => {
    let root;
    let repo;
    let client;
    let rows;
    before(async () => {
        root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-lib-lines-')));
        repo = path.join(root, 'repo');
        fs.mkdirSync(repo);
        fs.writeFileSync(path.join(repo, 'tsconfig.json'), JSON.stringify({
            compilerOptions: { target: 'es2022', lib: ['es2022', 'dom'], module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
            include: ['src/**/*.ts'],
        }));
        fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({ name: 'web' }));
        fs.mkdirSync(path.join(repo, 'src'));
        const main = path.join(repo, 'src', 'main.ts');
        fs.writeFileSync(main, SOURCE);
        client = new SidecarClient();
        await client.start();
        const ready = await client.send({
            action: 'init',
            request_id: 'init',
            repo_root: repo,
            tsconfig_path: 'tsconfig.json',
            scan_root: repo,
        });
        assert.strictEqual(ready.status, 'ready');
        const out = path.join(root, 'store', 'rows.jsonl.gz');
        const answer = await client.send({ action: 'position_rows', request_id: 'rows', files: [main], scan_root: repo, out_path: out, service: 'svc' }, 60_000);
        assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
        rows = zlib
            .gunzipSync(fs.readFileSync(out))
            .toString('utf8')
            .trimEnd()
            .split('\n')
            .slice(1)
            .flatMap((line) => JSON.parse(line).r);
    });
    after(async () => {
        await client.stop();
        fs.rmSync(root, { recursive: true, force: true });
    });
    /** The line of the `lib.dom.d.ts` declaration the call rows of `member` name. */
    const callLine = (member) => {
        const lines = new Set(rows.filter((r) => r[0] === 'call' && r[3] === member && r[4] === DOM).map((r) => r[5]));
        assert.strictEqual(lines.size, 1, `${member} call rows name lines ${[...lines]}`);
        const [line] = [...lines];
        declares(line, member);
        return line;
    };
    it('names each member call at its own line of lib.dom.d.ts', () => {
        const json = callLine('json');
        const randomUUID = callLine('randomUUID');
        const addEventListener = callLine('addEventListener');
        assert.strictEqual(new Set([json, randomUUID, addEventListener]).size, 3);
    });
    it('a verified op on BroadcastChannel names addEventListener at its own line, not response.json() or crypto.randomUUID()', async () => {
        const response = await client.send({
            request_id: 'verify',
            action: 'verify_library_claims',
            from_dir: repo,
            checks: [
                {
                    claim_id: 'channel',
                    package: DOM,
                    export: 'BroadcastChannel',
                    role: 'in_process_bus',
                    receiver: 'export',
                    claim: { kind: 'make', form: 'new', member: null, name: { arg: 0 } },
                },
                {
                    claim_id: 'listen',
                    package: DOM,
                    export: 'BroadcastChannel',
                    role: 'in_process_bus',
                    receiver: 'instance:new',
                    claim: { kind: 'op', op: 'receive', member: 'addEventListener', name: { arg: 0 } },
                },
            ],
        }, 60_000);
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        const [maker, verdict] = response.verdicts;
        assert.strictEqual(maker.verdict, 'verified', maker.reason);
        assert.strictEqual(verdict.verdict, 'verified', verdict.reason);
        const declarations = verdict.declarations ?? [];
        assert.ok(declarations.length > 0);
        for (const declared of declarations) {
            assert.strictEqual(declared.decl_file, DOM);
            declares(declared.decl_line, 'addEventListener');
        }
        // The label's declarations are the call rows' of the real listener and
        // of no other member.
        const named = new Set(declarations.map((d) => d.decl_line));
        assert.ok(named.has(callLine('addEventListener')));
        assert.ok(!named.has(callLine('json')));
        assert.ok(!named.has(callLine('randomUUID')));
    });
});
