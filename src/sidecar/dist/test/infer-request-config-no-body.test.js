/**
 * The `request_config` infer kind: a request call's options object literal,
 * and the member that carries its body. Where the literal writes nothing that
 * can be that member (no spread, no computed key, no member of that name in
 * any form, and no other body-named member), the call sends no request body,
 * and the inferrer decides it with the reason `no_request_body`. Anything else is not decided here.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const SEND_TS = `declare function send(url: string, init?: Record<string, unknown>): void;
declare const defaults: { headers: string };
declare const note: { id: number };
declare const key: string;
declare const options: { method: string };

send('/ping', { method: 'POST' });
send('/wraps', ({ method: 'POST' } as const));
send('/writes', { method: 'POST', body: JSON.stringify(note) });
send('/shorthand', { method: 'POST', body });
send('/quoted', { method: 'POST', 'body': undefined });
send('/spreads', { ...defaults, method: 'POST' });
send('/computed', { method: 'POST', [key]: 1 });
send('/method', { method() { return 1; }, body() { return 2; } });
send('/getter', { get body() { return 1; } });
send('/variable', options);
send('/other-member', { method: 'POST', payload: 1 });
send('/json', { method: 'POST', json: note });
send('/form', { method: 'POST', form: { a: '1' } });
send('/data', { method: 'POST', data: note });
send('/quoted-json', { method: 'POST', 'json': note });
declare const body: string;
`;
describe('request_config infer kind', () => {
    let client;
    let repoDir;
    let filePath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-request-config-'));
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', skipLibCheck: true },
            include: ['src'],
        }));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        filePath = path.join(repoDir, 'src', 'send.ts');
        fs.writeFileSync(filePath, SEND_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    /** The answer for the options literal of `send('<route>', ...)`. */
    const configAnswer = async (route, member = 'body') => {
        const callStart = SEND_TS.indexOf(`send('${route}'`);
        assert.ok(callStart >= 0, `fixture must call ${route}`);
        const open = SEND_TS.indexOf(', ', callStart) + 2;
        const end = SEND_TS.indexOf(');', open);
        const start = open;
        const line = SEND_TS.slice(0, callStart).split('\n').length;
        const alias = `Config_${route.replace(/\W/g, '_')}`;
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: filePath,
                    line_number: line,
                    span_start: start,
                    span_end: end,
                    infer_kind: 'request_config',
                    ...(member === null ? {} : { member }),
                    alias,
                },
            ],
        });
        return response.inferred_types?.find((t) => t.alias === alias);
    };
    it('decides no body for a literal that writes no body member', async () => {
        const answer = await configAnswer('/ping');
        assert.ok(answer, 'the decision must ride the row');
        assert.strictEqual(answer.type_string.trim(), 'unknown');
        assert.deepStrictEqual((answer.any_provenance ?? []).map((p) => [p.path, p.reason]), [['', 'no_request_body']]);
        assert.match(answer.any_provenance?.[0]?.detail ?? '', /'body'/);
    });
    it('reads through a parenthesised assertion', async () => {
        assert.ok(await configAnswer('/wraps'));
    });
    it('names the member it was sent', async () => {
        const answer = await configAnswer('/other-member', 'payload');
        assert.strictEqual(answer, undefined, 'the literal writes `payload`');
        const other = await configAnswer('/other-member', 'data');
        assert.match(other?.any_provenance?.[0]?.detail ?? '', /'data'/);
    });
    // A client labelled with one body member may take its body through another
    // (`json`, `form`, `data`), so a literal that writes any body-named key is
    // not decided, whichever member the locator names.
    for (const [route, member] of [
        ['/json', 'body'],
        ['/form', 'body'],
        ['/data', 'body'],
        ['/quoted-json', 'body'],
        ['/writes', 'data'],
        ['/json', 'payload'],
    ]) {
        it(`does not decide where the literal writes another body member (${route}, ${member})`, async () => {
            assert.strictEqual(await configAnswer(route, member), undefined);
        });
    }
    for (const route of [
        '/writes',
        '/shorthand',
        '/quoted',
        '/spreads',
        '/computed',
        '/method',
        '/getter',
        '/variable',
    ]) {
        it(`does not decide where the literal may carry the member (${route})`, async () => {
            assert.strictEqual(await configAnswer(route), undefined);
        });
    }
    it('does not decide without a member', async () => {
        assert.strictEqual(await configAnswer('/ping', null), undefined);
    });
});
