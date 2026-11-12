/**
 * carrick#2762: a request config literal that spreads another object is
 * decided "no request body" only when the spread's type is read and declares
 * no body member, or one typed `never` or `undefined`, in every branch.
 * A type parameter, an index signature, or any union branch with a body
 * member leaves it undecided. A literal's own quoted key or getter is a
 * written member, never "no body".
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const SRC = `interface Cfg<D = unknown> { url?: string; data?: D }
declare function send<D>(url: string, config?: Cfg<D>): void;
type NoBodyOptions = { data?: never; headers?: string };
type MaybeBodyOptions = { data?: { name: string }; headers?: string };
declare const noBody: NoBodyOptions;
declare const maybeBody: MaybeBodyOptions;
declare const untyped: any;
declare const bag: Record<string, unknown>;
declare const either: { data: { name: string } } | { headers: string };
declare const note: { name: string };

send('/plain', { url: '/plain' });
send('/never', { url: '/never', ...noBody });
send('/maybe', { url: '/maybe', ...maybeBody });
send('/untyped', { url: '/untyped', ...untyped });
function bare<T>(options: T) {
  send('/generic', { url: '/generic', ...options });
}
function bounded<T extends object>(options: T) {
  send('/generic-object', { url: '/generic-object', ...options });
}
send('/record', { url: '/record', ...bag });
send('/either', { url: '/either', ...either });
send('/dquoted', { url: '/dquoted', "data": note });
send('/getter', { url: '/getter', get data() { return note; } });
`;
describe('carrick#2762: a spread in a request config literal', () => {
    let client;
    let repoDir;
    let file;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2762-'));
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', skipLibCheck: true },
            include: ['src'],
        }));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        file = path.join(repoDir, 'src', 'send.ts');
        fs.writeFileSync(file, SRC);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    /** Reasons the row carries; undefined when the row is not answered. */
    const reasons = async (route) => {
        const row = await answer(route);
        return row ? (row.any_provenance ?? []).map((p) => p.reason) : undefined;
    };
    /** The row answered for the config literal of `send('<route>', ...)`. */
    const answer = async (route) => {
        const marker = `{ url: '${route}'`;
        const start = SRC.indexOf(marker);
        assert.ok(start >= 0, route);
        const end = SRC.indexOf(');', start);
        const alias = `Request_${route.slice(1)}`;
        const res = await client.send({
            action: 'infer',
            request_id: alias,
            requests: [
                {
                    file_path: file,
                    line_number: SRC.slice(0, start).split('\n').length,
                    span_start: start,
                    span_end: end,
                    infer_kind: 'request_body',
                    alias,
                },
            ],
        });
        return res.inferred_types?.find((t) => t.alias === alias);
    };
    it('no spread, no body member: decided no body (unchanged)', async () => {
        assert.deepStrictEqual(await reasons('/plain'), ['no_request_body']);
    });
    it('a spread whose body is never: decided no body', async () => {
        assert.deepStrictEqual(await reasons('/never'), ['no_request_body']);
    });
    it('a spread with an optional body: not decided', async () => {
        assert.ok(!(await reasons('/maybe'))?.includes('no_request_body'));
    });
    it('a spread of an untyped object: not decided', async () => {
        assert.ok(!(await reasons('/untyped'))?.includes('no_request_body'));
    });
    for (const route of ['/generic', '/generic-object', '/record', '/either']) {
        it(`a spread whose body member is not ruled out: not decided (${route})`, async () => {
            assert.ok(!(await reasons(route))?.includes('no_request_body'));
        });
    }
    it('a quoted body key is read as the body', async () => {
        assert.match((await answer('/dquoted'))?.type_string ?? '', /name: string/);
    });
    for (const route of ['/dquoted', '/getter']) {
        it(`a written body member is never no body (${route})`, async () => {
            assert.ok(!(await reasons(route))?.includes('no_request_body'));
        });
    }
});
