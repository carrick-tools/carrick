/**
 * An options parameter generic over what it serves, whose type is
 * an intersection with a conditional on the type parameter (the context
 * option is optional only when the served collection's context is empty).
 * Inside the generic signature that conditional stays deferred. The lister
 * listed no keys there, so a claim could not state `runs` at the key that
 * takes the collection; the verifier refused the same key. Both read one
 * predicate (`isObjectLike`), so a key listed is a key that verifies.
 *
 * The package is invented and its declarations are hand-written.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const TSCONFIG = JSON.stringify({
    compilerOptions: {
        target: 'es2020',
        module: 'commonjs',
        moduleResolution: 'node',
        strict: true,
        esModuleInterop: true,
        skipLibCheck: true,
        types: [],
    },
    include: ['src/**/*.ts'],
});
// A kit whose `collect` member gathers handlers by key into a collection, and
// adapters that serve a collection handed to them in an options object. The
// context option is optional only when the collection's context is empty: a
// conditional on the type parameter, so it stays deferred inside the generic
// signature. `serveFixed` names the collection type itself, `servePlain`
// writes no conditional. `serveOpen` and `serveUnknown` intersect a
// conditional whose branch says nothing (`any`, `unknown`); `serveInfer` one
// whose branch is a parameter it infers, which the checker's constraint drops.
const ADAPTER = `type OptionalWhen<C, T> = C extends true ? Partial<T> : T;
type ContextOption<TContext> = OptionalWhen<object extends TContext ? true : false, { makeContext: () => TContext }>;
type OpenWhen<C> = C extends true ? any : { strict: string };
type UnknownWhen<C> = C extends true ? unknown : { strict: string };
type InferWhen<C> = C extends { ctx: infer X } ? X : { strict: string };
export interface Collection {
  readonly isCollection: true;
  readonly _context: object;
}
export interface HandlerRecord {
  [key: string]: (input: unknown) => unknown;
}
export interface Kit {
  collect<R extends HandlerRecord>(record: R): Collection;
}
export interface Init {
  create(): Kit;
}
export declare const init: Init;
export type ServeOptions<TC extends Collection> = { collection: TC; endpoint: string } & ContextOption<TC["_context"]>;
export declare function serveGeneric<TC extends Collection>(opts: ServeOptions<TC>): void;
export declare function serveFixed(opts: ServeOptions<Collection>): void;
export declare function servePlain<TC extends Collection>(opts: { collection: TC; endpoint: string }): void;
export declare function serveOpen<TC extends Collection>(
  opts: { collection: TC; endpoint: string } & OpenWhen<object extends TC["_context"] ? true : false>
): void;
export declare function serveUnknown<TC extends Collection>(
  opts: { collection: TC; endpoint: string } & UnknownWhen<object extends TC["_context"] ? true : false>
): void;
export declare function serveInfer<TC extends Collection>(opts: { collection: TC; endpoint: string } & InferWhen<TC>): void;
type SelfLoop<C> = C extends string ? SelfLoop<C> : { strict: string };
type SelfFlat<C> = C extends Array<infer U> ? SelfFlat<U> : C;
type PingWhen<C> = C extends string ? PongWhen<C> : { strict: string };
type PongWhen<C> = C extends number ? PingWhen<C> : { strict: number };
export declare function serveSelf<TC extends Collection>(
  opts: { collection: TC; endpoint: string } & SelfLoop<TC["_context"]>
): void;
export declare function serveFlat<TC extends Collection>(
  opts: { collection: TC; endpoint: string } & SelfFlat<TC["_context"]>
): void;
export declare function serveMutual<TC extends Collection>(
  opts: { collection: TC; endpoint: string } & PingWhen<TC["_context"]>
): void;
`;
// What the lister of origin/main 3041924a listed for the two adapters with no
// deferred conditional: unchanged byte for byte.
const FIXED_BEFORE = '{"name":"opts","optional":false,"rest":false,"type":"ServeOptions<Collection>","accepts_string":false,"function":false,' +
    '"keys":[{"name":"collection","optional":false,"accepts_string":false,"function":false},' +
    '{"name":"endpoint","optional":false,"accepts_string":true,"function":false},' +
    '{"name":"makeContext","optional":true,"accepts_string":false,"function":true}]}';
const PLAIN_BEFORE = '{"name":"opts","optional":false,"rest":false,"type":"{ collection: TC; endpoint: string; }","accepts_string":false,"function":false,' +
    '"keys":[{"name":"collection","optional":false,"accepts_string":false,"function":false},' +
    '{"name":"endpoint","optional":false,"accepts_string":true,"function":false}]}';
const P = 'fixture-adapter';
describe('list_library_surface: options intersected with a deferred conditional', () => {
    let client;
    let root;
    let requestId = 0;
    async function surface() {
        const response = await client.send({ request_id: `s-${requestId++}`, action: 'list_library_surface', from_dir: root, packages: [P] }, 60_000);
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        return response.surfaces[0];
    }
    /** The first parameter of an export's only call signature. */
    const firstParam = (listed, exported) => {
        const entry = listed.exports.find(e => e.export === exported);
        assert.ok(entry, `${exported} is not listed`);
        const calls = entry.receivers.flatMap(r => r.call ?? []);
        assert.strictEqual(calls.length, 1);
        return calls[0].params[0];
    };
    const keyNames = (param) => (param.keys ?? []).map(k => k.name);
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-conditional-'));
        const files = {
            'tsconfig.json': TSCONFIG,
            'package.json': JSON.stringify({ name: 'svc' }),
            'src/index.ts': 'export const service = 1;\n',
            [`node_modules/${P}/package.json`]: JSON.stringify({ name: P, version: '1.0.0', types: 'index.d.ts' }),
            [`node_modules/${P}/index.d.ts`]: ADAPTER,
        };
        for (const [rel, text] of Object.entries(files)) {
            const file = path.join(root, rel);
            fs.mkdirSync(path.dirname(file), { recursive: true });
            fs.writeFileSync(file, text);
        }
        client = new SidecarClient();
        await client.start();
        const init = await client.send({ request_id: 'init', action: 'init', repo_root: root });
        assert.strictEqual(init.status, 'ready');
    });
    after(async () => {
        await client.stop();
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('lists the keys of a generic options intersection with a deferred conditional part', async () => {
        const listed = await surface();
        const param = firstParam(listed, 'serveGeneric');
        assert.deepStrictEqual(keyNames(param), ['collection', 'endpoint', 'makeContext']);
        const byName = new Map((param.keys ?? []).map(k => [k.name, k]));
        assert.strictEqual(byName.get('endpoint').accepts_string, true);
        assert.strictEqual(byName.get('endpoint').optional, false);
        assert.strictEqual(byName.get('makeContext').function, true);
        // Optional when the context is empty: some branch marks it optional.
        assert.strictEqual(byName.get('makeContext').optional, true);
    });
    it('lists the same keys it listed before where no deferred conditional is present', async () => {
        const listed = await surface();
        assert.strictEqual(JSON.stringify(firstParam(listed, 'serveFixed')), FIXED_BEFORE);
        assert.strictEqual(JSON.stringify(firstParam(listed, 'servePlain')), PLAIN_BEFORE);
    });
    it('lists nothing where a branch of the conditional says nothing, or is inferred', async () => {
        const listed = await surface();
        for (const exported of ['serveOpen', 'serveUnknown', 'serveInfer']) {
            assert.strictEqual(firstParam(listed, exported).keys, undefined, exported);
        }
    });
    // A conditional whose branch is the conditional again (itself, or through a
    // second alias) must end the read, not recurse. The checker's constraint
    // already ends these chains, so the listing is what it resolves; the guard
    // (`MAX_CONDITIONAL_DEPTH`) is the backstop, and this pins that such options
    // finish and list the keys the terminating branch gives.
    it('finishes where the conditional recurses into itself or a sibling', { timeout: 30_000 }, async () => {
        const listed = await surface();
        assert.deepStrictEqual(keyNames(firstParam(listed, 'serveSelf')), ['collection', 'endpoint', 'strict']);
        assert.strictEqual(firstParam(listed, 'serveFlat').keys, undefined);
        assert.deepStrictEqual(keyNames(firstParam(listed, 'serveMutual')), ['collection', 'endpoint', 'strict']);
    });
    it('a maker that runs the collection at the listed key verifies', async () => {
        let sequence = 0;
        const check = (exported, receiver, claim) => ({
            claim_id: `c${sequence++}`,
            package: P,
            export: exported,
            role: 'server_framework',
            receiver,
            claim,
        });
        const KIT = 'instance:create';
        const adapter = (exported) => check(exported, 'export', {
            kind: 'make',
            form: 'call',
            member: null,
            runs: { arg: 0, key: 'collection' },
            prefix: { arg: 0, key: 'endpoint' },
        });
        const response = await client.send({
            request_id: `v-${requestId++}`,
            action: 'verify_library_claims',
            from_dir: root,
            checks: [
                check('init', 'export', { kind: 'make', form: 'call', member: 'create' }),
                check('init', KIT, { kind: 'scope', member: 'collect', of: KIT, collects: { arg: 0, join: '.' } }),
                adapter('serveGeneric'),
                adapter('serveFixed'),
                adapter('servePlain'),
                adapter('serveOpen'),
                adapter('serveInfer'),
            ],
        }, 60_000);
        assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
        const verdicts = response.verdicts.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
        // The two listing nothing are refused at the argument, as before.
        assert.deepStrictEqual(verdicts.slice(2), [
            'verified',
            'verified',
            'verified',
            'failed param_missing',
            'failed param_missing',
        ]);
    });
});
