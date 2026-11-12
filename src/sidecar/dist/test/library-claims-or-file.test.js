/**
 * A handler slot that takes the function or the path of a file exporting it
 * (QW-1, `handler: { arg, key?, or_file: true }`).
 *
 * - With `or_file`, the verifier drops the slot's file parts (string-like,
 *   or the platform `URL`) and judges the rest by the handler rule: at least
 *   one typed function must remain.
 * - A constructor whose second argument is options (`new Queue(name, opts)`)
 *   still fails: an options object is no file part.
 * - The lister marks such a parameter `function_or_file: true`, and writes
 *   nothing new on any other parameter.
 * - `or_file` on any slot but a make's or an op's `handler` is `claim_invalid`.
 *
 * The packages are invented.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import { parseRequest } from '../src/validators.js';
const TSCONFIG = JSON.stringify({
    compilerOptions: {
        target: 'es2020',
        module: 'commonjs',
        moduleResolution: 'node',
        lib: ['es2020', 'dom'],
        strict: true,
        esModuleInterop: true,
        skipLibCheck: true,
        types: ['node'],
    },
    include: ['src/**/*.ts'],
});
const JOBS = `import { URL as NodeURL } from 'url';
import { URL as OwnURL } from './own-url';
export interface Job { id: string }
export type Processor = (job: Job) => Promise<unknown>;
export interface WorkerOptions { prefix?: string; concurrency?: number }
export declare class Worker {
  constructor(name: string, processor?: string | URL | null | Processor, opts?: WorkerOptions);
}
export declare class NodeWorker {
  constructor(name: string, processor?: string | NodeURL | null | Processor, opts?: WorkerOptions);
}
export declare class Queue {
  constructor(name: string, opts?: { prefix?: string });
}
export declare class QueueEvents {
  constructor(name: string, opts?: { prefix?: string });
}
export declare class OwnUrlWorker {
  constructor(name: string, processor?: string | OwnURL | Processor);
}
export declare class OptsOrFn {
  constructor(name: string, handler?: { concurrency?: number } | Processor);
}
export declare class PathOnly {
  constructor(name: string, file: string | NodeURL);
}
export declare class PlainFn {
  constructor(name: string, processor: Processor);
}
export declare class Untyped {
  constructor(name: string, processor: string | ((...args: any[]) => any));
}
export declare class KeyedWorker {
  constructor(options: { queue: string; processor: string | Processor; concurrency?: number });
}
export declare class Bus {
  constructor(name: string);
  publish(topic: string, payload: unknown): void;
  subscribe(topic: string, handler: (message: unknown) => void, ack?: () => void): void;
}
`;
const NODE_URL = `declare module 'url' {
  class URL { href: string; constructor(input: string) }
}
`;
const PKG = '@fixture/jobs';
const LABELS = { prefix: 'not_name' };
let sequence = 0;
const check = (exported, claim, receiver = 'export') => ({ claim_id: `o${sequence++}`, package: PKG, export: exported, role: 'broker', receiver, claim });
const newReceive = (exported, extra) => check(exported, { kind: 'op', op: 'receive', member: null, on: 'export', form: 'new', name: { arg: 0 }, ...extra });
const flagged = (exported, extra = {}) => newReceive(exported, { handler: { arg: 1, or_file: true }, key_labels: LABELS, ...extra });
describe('verify_library_claims: a handler that is a function or a file (QW-1)', () => {
    let root;
    let verifier;
    before(() => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-or-file-'));
        const files = {
            'tsconfig.json': TSCONFIG,
            'package.json': JSON.stringify({ name: 'svc' }),
            'src/index.ts': 'export const service = 1;\n',
            [`node_modules/${PKG}/package.json`]: JSON.stringify({ name: PKG, version: '5.1.0', types: 'index.d.ts' }),
            [`node_modules/${PKG}/index.d.ts`]: JOBS,
            // A package's own class named `URL` is no file part.
            [`node_modules/${PKG}/own-url.d.ts`]: 'export declare class URL { href: string }\n',
            'node_modules/@types/node/package.json': JSON.stringify({ name: '@types/node', version: '22.0.0', types: 'index.d.ts' }),
            'node_modules/@types/node/index.d.ts': '/// <reference path="url.d.ts" />\n',
            'node_modules/@types/node/url.d.ts': NODE_URL,
        };
        for (const [rel, text] of Object.entries(files)) {
            const file = path.join(root, rel);
            fs.mkdirSync(path.dirname(file), { recursive: true });
            fs.writeFileSync(file, text);
        }
        verifier = new LibraryClaimsVerifier(new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') }));
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    const verdicts = (checks) => verifier.run(root, checks, 60_000).semantics.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
    const verdict = (one) => verdicts([one])[0];
    it('verifies a constructed receive whose processor is a function, a path or a URL', () => {
        assert.strictEqual(verdict(flagged('Worker')), 'verified');
        assert.strictEqual(verdict(flagged('NodeWorker')), 'verified');
    });
    it('judges the unflagged handler as before', () => {
        assert.strictEqual(verdict(newReceive('Worker', { handler: { arg: 1 }, key_labels: LABELS })), 'failed handler_not_function');
        assert.strictEqual(verdict(newReceive('Worker', { handler: { arg: 1, or_file: false }, key_labels: LABELS })), 'failed handler_not_function');
    });
    it('still refuses a constructor that takes options where the handler would be', () => {
        assert.strictEqual(verdict(flagged('Queue')), 'failed handler_not_function');
        assert.strictEqual(verdict(flagged('QueueEvents')), 'failed handler_not_function');
        assert.strictEqual(verdict(flagged('OptsOrFn')), 'failed handler_not_function');
    });
    it('drops only string parts and the platform URL, never a package class of that name', () => {
        assert.strictEqual(verdict(flagged('OwnUrlWorker')), 'failed handler_not_function');
    });
    it('wants a function to remain once the file parts are dropped', () => {
        assert.strictEqual(verdict(flagged('PathOnly')), 'failed handler_not_function');
        assert.strictEqual(verdict(flagged('Untyped')), 'unchecked handler_untyped');
    });
    it('verifies a flagged slot that takes only a function (the rest is all function)', () => {
        assert.strictEqual(verdict(flagged('PlainFn')), 'verified');
    });
    it('judges a flagged keyed handler the same way', () => {
        const keyed = (handler) => check('KeyedWorker', {
            kind: 'op',
            op: 'receive',
            member: null,
            on: 'export',
            form: 'new',
            name: { arg: 0, key: 'queue' },
            handler: { arg: 0, key: 'processor', ...handler },
        });
        assert.strictEqual(verdict(keyed({ or_file: true })), 'verified');
        assert.strictEqual(verdict(keyed({})), 'failed handler_not_function');
    });
    it('is claim_invalid on any slot but a handler', () => {
        const subscribe = (extra) => check('Bus', { kind: 'op', op: 'receive', member: 'subscribe', of: 'instance:new', name: { arg: 0 }, handler: { arg: 1 }, ...extra }, 'instance:new');
        assert.deepStrictEqual(verdicts([
            newReceive('Worker', { name: { arg: 0, or_file: true }, handler: { arg: 1, or_file: true }, key_labels: LABELS }),
            subscribe({ ack: { arg: 2, or_file: true } }),
            check('Bus', { kind: 'op', op: 'send', member: 'publish', of: 'instance:new', name: { arg: 0 }, payload: { arg: 1, or_file: true } }, 'instance:new'),
            check('Bus', { kind: 'make', form: 'new', member: null, name: { arg: 0, or_file: true } }),
            check('Worker', { kind: 'scope', member: null, on: 'export', form: 'new', name: { arg: 0 }, mounts: { arg: 1, or_file: true } }),
        ]), Array(5).fill('unchecked claim_invalid'));
    });
    it('takes the flag on the wire', () => {
        const request = (slot) => parseRequest({
            request_id: 'r',
            action: 'verify_library_claims',
            from_dir: '/x',
            checks: [{ claim_id: 'c', package: PKG, export: 'Worker', role: 'broker', receiver: 'export', claim: { kind: 'op', op: 'receive', member: null, on: 'export', form: 'new', name: { arg: 0 }, handler: slot } }],
        }).success;
        assert.strictEqual(request({ arg: 1, or_file: true }), true);
        assert.strictEqual(request({ arg: 1, or_file: 'yes' }), false);
    });
    it('lists a function-or-file parameter, and nothing new on any other', () => {
        const listed = verifier.listSurface(root, [PKG], 1_000_000);
        const param = (exported, index) => {
            const entry = listed.surfaces[0].exports.find(e => e.export === exported);
            return entry.receivers.find(r => r.receiver === 'export').construct[0].params[index];
        };
        for (const exported of ['Worker', 'NodeWorker']) {
            assert.strictEqual(param(exported, 1).function, false, exported);
            assert.strictEqual(param(exported, 1).function_or_file, true, exported);
        }
        for (const [exported, index] of [
            ['Worker', 0],
            ['Worker', 2],
            ['Queue', 1],
            ['QueueEvents', 1],
            ['OwnUrlWorker', 1],
            ['OptsOrFn', 1],
            ['PathOnly', 1],
            ['PlainFn', 1],
            ['Untyped', 1],
        ]) {
            assert.ok(!('function_or_file' in param(exported, index)), `${exported} ${index}`);
        }
    });
});
