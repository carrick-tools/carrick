/**
 * Blueprint rows N27 (i) and N30 (carrick#3334, carrick#3336): the compiler,
 * not text, decides what a consumer call's model symbol and stated body name.
 *
 * N27 (i): where a call's result is untyped (`client.post(url, event)` on an
 * untyped client), the model sometimes names the type of the value the call
 * SENDS (`AuditEvent`) as the type it receives. The compiler relates the
 * model's symbol to each argument of the call; a symbol that is an
 * argument's type, or its element, is answered `different`, and the driver
 * drops it so the inference defines the alias.
 *
 * N30: where the source states the body it reads (`as readonly User[]`), the
 * stated body carries how its root's declared type relates to the statement
 * (`root_relation`). The driver re-aims at the root on `same` or `list`,
 * whatever the printed text; it never compares the text.
 *
 * Every request goes through the process over stdio, so a field the schema
 * dropped would show as a missing answer.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const SOURCE_TS = `export interface AuditEvent { id: string; kind: string }
export interface Receipt { accepted: boolean }
export interface User { id: string }
export interface Page<T> { items: T[] }
export type Users = User[];

declare const client: any;
declare const typed: { post(url: string, body: AuditEvent): Promise<Receipt> };
declare const url: string;

export async function sendsEvent(event: AuditEvent) {
  const res = await client.post(url, event); // sends-event
  return res;
}
export async function sendsEvents(events: AuditEvent[]) {
  const res = await client.post(url, events); // sends-events
  return res;
}
export async function typedResult(event: AuditEvent) {
  const res = await typed.post(url, event); // typed-result
  return res;
}
export async function readonlyList() {
  const res = await fetch(url + '/readonly-list'); // readonly-list
  return (await res.json()) as readonly User[];
}
export async function plainRoot() {
  const res = await fetch(url + '/plain-root'); // plain-root
  return (await res.json()) as User;
}
export async function aliasOfList() {
  const res = await fetch(url + '/alias-of-list'); // alias-of-list
  return (await res.json()) as Users;
}
export async function orNull() {
  const res = await fetch(url + '/or-null'); // or-null
  return (await res.json()) as User | null;
}
export async function genericRoot() {
  const res = await fetch(url + '/generic-root'); // generic-root
  return (await res.json()) as Page<User>;
}
`;
describe('N27 (i), N30: a consumer call result is related by the compiler', () => {
    let client;
    let repoDir;
    let sourcePath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-n27-'));
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                lib: ['es2022', 'dom'],
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        sourcePath = path.join(repoDir, 'src', 'calls.ts');
        fs.writeFileSync(sourcePath, SOURCE_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    const lineOf = (marker) => {
        const index = SOURCE_TS.split('\n').findIndex((line) => line.includes(marker));
        assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
        return index + 1;
    };
    const infer = async (marker, call, symbol) => {
        const alias = `Alias_${marker.replace(/\W/g, '_')}_${symbol ?? 'none'}`;
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: sourcePath,
                    line_number: lineOf(`// ${marker}`),
                    expression_text: call,
                    infer_kind: 'call_result',
                    alias,
                },
            ],
            ...(symbol ? { model_symbols: [{ alias, symbol_name: symbol, source_file: 'src/calls.ts' }] } : {}),
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        assert.ok(inferred, `no answer for ${marker}`);
        return inferred;
    };
    // N27 (i)
    it("answers different where the untyped call's model symbol is the type it sends", async () => {
        const inferred = await infer('sends-event', 'client.post(url, event)', 'AuditEvent');
        assert.strictEqual(inferred.model_symbol_relation?.relation, 'different', inferred.type_string);
    });
    it('answers different where the symbol is the element of a list the call sends', async () => {
        const inferred = await infer('sends-events', 'client.post(url, events)', 'AuditEvent');
        assert.strictEqual(inferred.model_symbol_relation?.relation, 'different', inferred.type_string);
    });
    it('answers nothing where the symbol is not what the call sends', async () => {
        const inferred = await infer('sends-event', 'client.post(url, event)', 'Receipt');
        assert.strictEqual(inferred.model_symbol_relation, undefined);
    });
    it('answers nothing where the result is typed, even if the symbol is what the call sends', async () => {
        const inferred = await infer('typed-result', 'typed.post(url, event)', 'AuditEvent');
        assert.strictEqual(inferred.model_symbol_relation, undefined, inferred.type_string);
    });
    // N30
    it("relates a stated readonly list to its root as a list, the list's readonly set aside", async () => {
        const inferred = await infer('readonly-list', "fetch(url + '/readonly-list')");
        assert.strictEqual(inferred.stated_body?.root, 'User');
        assert.deepStrictEqual(inferred.stated_body?.root_relation, { relation: 'list', depth: 1 });
    });
    it('relates a stated root to itself as the same', async () => {
        const plain = await infer('plain-root', "fetch(url + '/plain-root')");
        assert.deepStrictEqual(plain.stated_body?.root_relation, { relation: 'same' });
        const alias = await infer('alias-of-list', "fetch(url + '/alias-of-list')");
        assert.strictEqual(alias.stated_body?.root, 'Users');
        assert.deepStrictEqual(alias.stated_body?.root_relation, { relation: 'same' });
    });
    it('relates a root with null beside it as nullable', async () => {
        const inferred = await infer('or-null', "fetch(url + '/or-null')");
        assert.deepStrictEqual(inferred.stated_body?.root_relation, { relation: 'nullable' });
    });
    it("relates a generic root to its instance as different: the root alone loses the argument", async () => {
        const inferred = await infer('generic-root', "fetch(url + '/generic-root')");
        assert.strictEqual(inferred.stated_body?.root, 'Page');
        assert.deepStrictEqual(inferred.stated_body?.root_relation, { relation: 'different' });
    });
});
