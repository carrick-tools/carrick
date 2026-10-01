/**
 * carrick#1616 (prototype): `verify_library_claims` checks claims of every
 * role in the shared shape against the package's own declarations, and the
 * role picks the checks.
 *
 * Every package below is invented and its declarations are hand-written. The
 * three answered packages mirror the slice: a task SDK (`broker`: a definition
 * maker whose options carry the name and the handler, instance sends with the
 * name bound by the maker, export sends with the name at argument 0), a
 * key-value store's pub/sub client (`broker`: publish and subscribe, topic at
 * argument 0, handler at argument 1, its client re-exported from a core
 * package), and a socket package (`socket`: a client maker and a server class,
 * `emit` and `on` with the event at argument 0, library-owned names declared
 * as literals). The rest isolate one must-not-verify rule each (design record
 * 2026-10-01, section 5), so deleting a rule flips its own fixture.
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
  include: ['src/**/*.ts', 'types/**/*.d.ts'],
});

const TASKS = `export interface RunContext {
  attempt: number;
}
export interface TaskOptions<TPayload> {
  id: string;
  run: (payload: TPayload, context: RunContext) => Promise<unknown>;
  retries?: number;
}
export interface TriggerOptions {
  delay?: number;
  tags?: string[];
}
export interface RunHandle {
  id: string;
}
export interface Task<TPayload> {
  id: string;
  trigger(payload: TPayload, options?: TriggerOptions): Promise<RunHandle>;
}
export declare function task<TPayload = unknown>(options: TaskOptions<TPayload>): Task<TPayload>;
// Two maker overloads that both take the claim, building different instances.
export interface ScheduledTask {
  id: string;
  trigger(payload: unknown): Promise<RunHandle>;
  cancel(): void;
}
export declare function job(options: { id: string; run: (payload: unknown) => Promise<unknown>; cron: number }): Task<unknown>;
export declare function job(options: { id: string; run: (payload: unknown) => Promise<unknown> }): ScheduledTask;
export declare const tasks: {
  trigger<TPayload = unknown>(id: string, payload: TPayload, options?: TriggerOptions): Promise<RunHandle>;
};
`;

// The key-value client lives in a core package; the named package re-exports it.
const KV_CORE = `export type Listener = (message: string, channel: string) => void;
export interface ClientOptions {
  url?: string;
}
export interface Client {
  connect(): Promise<this>;
  publish(channel: string, message: string | Uint8Array): Promise<number>;
  subscribe(channel: string, listener: Listener): Promise<void>;
  get(key: string): Promise<string | null>;
}
export declare function createClient(options?: ClientOptions): Client;
`;

const SOCKET = `export type Ack = (response: unknown) => void;
export type Listener = (payload: unknown, ack?: Ack) => void;
export interface ClientSocket {
  emit(event: string, payload?: unknown, ack?: Ack): boolean;
  on(event: 'connect' | 'disconnect', listener: () => void): this;
  on(event: string, listener: Listener): this;
}
export interface ServerSocket {
  emit(event: string, payload?: unknown): boolean;
  on(event: string, listener: Listener): this;
}
export declare function io(url: string, options?: { path?: string }): ClientSocket;
export declare class Server {
  constructor(options?: { port?: number });
  on(event: 'connection', listener: (socket: ServerSocket) => void): this;
  emit(event: string, payload?: unknown): boolean;
}
`;

// The runtime's event emitter, as its type package declares it.
const RUNTIME_EVENTS = `declare module 'events' {
  class EventEmitter {
    emit(eventName: string | symbol, ...args: any[]): boolean;
    on(eventName: string | symbol, listener: (...args: any[]) => void): this;
  }
  export = EventEmitter;
}
`;

// A library class that extends the runtime's emitter and adds one own member.
const BUS = `/// <reference types="node" />
import EventEmitter = require('events');
export declare class Bus extends EventEmitter {
  publishLocal(topic: string, payload: unknown): void;
}
export declare function createBus(): EventEmitter;
`;

// A base class another package declares, and a package that extends it.
const BASE_EMITTER = `export declare class Emitter {
  send(topic: string, payload: unknown): void;
}
`;
const DERIVED = `import { Emitter } from '@fixture/base-emitter';
export declare class Relay extends Emitter {
  forward(topic: string, payload: unknown): void;
}
`;

// One member per name-slot, handler and maker rule.
const RULES = `export type Handler = (message: string) => void;
export interface EventMap {
  [event: string]: unknown;
}
export interface Channel<Events extends EventMap = EventMap> {
  send(event: string, payload: unknown): void;
  emitKey<K extends keyof Events>(event: K, payload: unknown): void;
  sendKey(event: keyof EventMap, payload: unknown): void;
  trigger(channel: string, event: string, data: unknown): void;
  publish(topic: string, message: string): void;
  publishWithMeta(topic: string, message: unknown, meta?: string): void;
  publishMany(topic: string, ...messages: string[]): void;
  subscribeAll(...topics: string[]): void;
  push(data: string): void;
  onAny(topic: string, handler: any): void;
  onUnknown(topic: string, handler: unknown): void;
  onFunction(topic: string, handler: Function): void;
  onRest(topic: string, handler: (...args: any[]) => void): void;
  onOptions(topic: string, options: { retry?: number }): void;
  onTyped(topic: string, handler: Handler): void;
  define(options: { id: string; run: Handler }): void;
  defineDescribed(options: { id: string; description?: string; run: Handler }): void;
  defineSplit(options: { id: string } | { run: Handler }): void;
}
export interface Definition<Id extends string> {
  id: Id;
}
export type IdOf<D> = D extends Definition<infer Id> ? Id : never;
export type IdOrAny<D> = D extends Definition<infer Id> ? Id : any;
export interface Dispatcher {
  triggerById<D extends Definition<string>>(id: IdOf<D>, payload: unknown): void;
  triggerLoose<D extends Definition<string>>(id: IdOrAny<D>, payload: unknown): void;
  subscribeMany(...args: [...channels: string[], callback: (err: Error | null) => void]): void;
  sendRaw(data: string, callback?: (err?: Error) => void): void;
  ping(event: string): void;
}
export declare const channel: Channel;
export declare const dispatcher: Dispatcher;
export declare function makeAny(): any;
export declare function makeOpen<T>(): T;
export declare function makeChannel(): Channel;
`;

const ANY_EXPORT = `declare const client: any;
export default client;
export declare const vague: unknown;
`;

function writeTree(root: string, files: Record<string, string>): void {
  for (const [rel, text] of Object.entries(files)) {
    const file = path.join(root, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  }
}

function packageJson(name: string, version: string, entry: Record<string, unknown>): string {
  return JSON.stringify({ name, version, ...entry });
}

type Slot = { arg: number; key?: string };
type Claim = Record<string, unknown> & { list: string };
interface Check {
  claim_id: string;
  package: string;
  export: string;
  role: string;
  side?: string;
  receiver: string;
  claim: Claim;
}
interface Result {
  claim_id: string;
  receiver: string;
  verdict: 'verified' | 'failed' | 'unchecked';
  reason?: string;
}
interface Response {
  status: string;
  semantics?: Result[];
  semantics_modules?: Array<Record<string, unknown>>;
  errors?: string[];
}

let sequence = 0;
function check(pkg: string, exported: string, role: string, receiver: string, claim: Claim): Check {
  return { claim_id: `c${sequence++}`, package: pkg, export: exported, role, receiver, claim };
}
const makes = (form: 'call' | 'new', member: string | null, parts: Record<string, Slot> = {}): Claim => ({
  list: 'makes',
  form,
  member,
  ...parts,
});
const op = (opKind: string, member: string | null, parts: Record<string, unknown>): Claim => ({
  list: 'ops',
  op: opKind,
  member,
  ...parts,
});

function verdicts(response: Response): string[] {
  assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
  return response.semantics!.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
}

describe('verify_library_claims (carrick#1616 prototype)', () => {
  let root: string;
  let client: SidecarClient;
  let requestId = 0;

  const verify = (checks: Check[], variants?: string[]) =>
    client.send<Response>(
      {
        request_id: `claims-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: root,
        checks,
        ...(variants === undefined ? {} : { variants }),
      },
      60_000
    );

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-'));
    writeTree(root, {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({
        name: 'worker',
        dependencies: { '@fixture/tasks': '^4.0.0', 'fixture-local-bus': 'file:./vendor/local-bus' },
      }),
      'src/index.ts': 'export const service = 1;\n',
      'types/shims.d.ts': "declare module 'fixture-shorthand-bus';\n",
      // The service adds a member to the key-value client for itself.
      'src/augment.ts':
        "import '@fixture/kv-core';\ndeclare module '@fixture/kv-core' {\n  interface Client {\n    broadcast(channel: string, message: string): Promise<number>;\n  }\n}\n",
      'node_modules/@fixture/tasks/package.json': packageJson('@fixture/tasks', '4.0.0', { types: 'index.d.ts' }),
      'node_modules/@fixture/tasks/index.d.ts': TASKS,
      'node_modules/@fixture/kv-core/package.json': packageJson('@fixture/kv-core', '5.1.0', { types: 'index.d.ts' }),
      'node_modules/@fixture/kv-core/index.d.ts': KV_CORE,
      'node_modules/fixture-kv/package.json': packageJson('fixture-kv', '5.1.0', { types: 'index.d.ts' }),
      'node_modules/fixture-kv/index.d.ts': "export * from '@fixture/kv-core';\n",
      'node_modules/fixture-socket/package.json': packageJson('fixture-socket', '2.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-socket/index.d.ts': SOCKET,
      'node_modules/@types/node/package.json': packageJson('@types/node', '22.0.0', { types: 'index.d.ts' }),
      'node_modules/@types/node/index.d.ts': RUNTIME_EVENTS,
      'node_modules/fixture-bus/package.json': packageJson('fixture-bus', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-bus/index.d.ts': BUS,
      'node_modules/@fixture/base-emitter/package.json': packageJson('@fixture/base-emitter', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/@fixture/base-emitter/index.d.ts': BASE_EMITTER,
      'node_modules/fixture-derived/package.json': packageJson('fixture-derived', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-derived/index.d.ts': DERIVED,
      'node_modules/fixture-rules/package.json': packageJson('fixture-rules', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-rules/index.d.ts': RULES,
      'node_modules/fixture-any-bus/package.json': packageJson('fixture-any-bus', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-any-bus/index.d.ts': ANY_EXPORT,
      // Installed from the service's own source by a `file:` range.
      'node_modules/fixture-local-bus/package.json': packageJson('fixture-local-bus', '0.0.1', { types: 'index.d.ts' }),
      'node_modules/fixture-local-bus/index.d.ts': 'export declare function publish(topic: string, payload: unknown): void;\n',
    });
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  // --------------------------------------------------------------------------
  // The slice's packages
  // --------------------------------------------------------------------------

  it('verifies the task SDK: the definition maker, sends bound to its name, and export sends', async () => {
    const T = '@fixture/tasks';
    const checks = [
      check(T, 'task', 'broker', 'export', makes('call', null, { name: { arg: 0, key: 'id' }, handler: { arg: 0, key: 'run' } })),
      check(T, 'task', 'broker', 'instance:()', op('send', 'trigger', { name: { bound: 'maker' }, payload: { arg: 0 } })),
      check(T, 'tasks', 'broker', 'export', op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['verified', 'verified', 'verified']);
  });

  it('reads an instance only through a maker claim that holds in the same request', async () => {
    const T = '@fixture/tasks';
    const send = op('send', 'trigger', { name: { bound: 'maker' }, payload: { arg: 0 } });
    assert.deepStrictEqual(verdicts(await verify([check(T, 'task', 'broker', 'instance:()', send)])), [
      'unchecked maker_unverified',
    ]);
    // A maker claim that does not hold builds no instance.
    const wrongMaker = makes('call', null, { name: { arg: 0, key: 'name' } });
    assert.deepStrictEqual(
      verdicts(await verify([check(T, 'task', 'broker', 'export', wrongMaker), check(T, 'task', 'broker', 'instance:()', send)])),
      ['failed key_missing', 'unchecked maker_unverified']
    );
    // A name bound by a maker that binds none.
    assert.deepStrictEqual(
      verdicts(
        await verify([
          check(T, 'task', 'broker', 'export', makes('call', null, { handler: { arg: 0, key: 'run' } })),
          check(T, 'task', 'broker', 'instance:()', send),
        ])
      ),
      ['verified', 'failed name_unbound']
    );
  });

  it('reads an instance through every maker overload that holds, and needs the op on each', async () => {
    const T = '@fixture/tasks';
    const maker = makes('call', null, { name: { arg: 0, key: 'id' }, handler: { arg: 0, key: 'run' } });
    const checks = [
      check(T, 'job', 'broker', 'export', maker),
      check(T, 'job', 'broker', 'instance:()', op('send', 'trigger', { name: { bound: 'maker' }, payload: { arg: 0 } })),
      // Only one overload's instance can cancel.
      check(T, 'job', 'broker', 'instance:()', op('send', 'cancel', { name: { bound: 'maker' }, payload: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['verified', 'verified', 'failed member_missing']);
  });

  it('verifies the key-value client re-exported from its core package', async () => {
    const K = 'fixture-kv';
    const checks = [
      check(K, 'createClient', 'broker', 'export', makes('call', null, { base: { arg: 0, key: 'url' } })),
      check(K, 'createClient', 'broker', 'instance:()', op('send', 'publish', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(K, 'createClient', 'broker', 'instance:()', op('receive', 'subscribe', { name: { arg: 0 }, handler: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['verified', 'verified', 'verified']);
  });

  it('verifies the socket client and server, and the names they declare for themselves', async () => {
    const S = 'fixture-socket';
    const checks = [
      check(S, 'io', 'socket', 'export', makes('call', null, { base: { arg: 0 } })),
      check(S, 'io', 'socket', 'instance:()', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 }, ack: { arg: 2 } })),
      check(S, 'io', 'socket', 'instance:()', op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 } })),
      check(S, 'io', 'socket', 'instance:()', { list: 'reserved', name: 'connect', member: 'on', at: { arg: 0 } }),
      check(S, 'io', 'socket', 'instance:()', { list: 'reserved', name: 'reconnect', member: 'on', at: { arg: 0 } }),
      check(S, 'Server', 'socket', 'export', makes('new', null)),
      check(S, 'Server', 'socket', 'instance:new', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(S, 'Server', 'socket', 'instance:new', { list: 'reserved', name: 'connection', member: 'on', at: { arg: 0 } }),
    ].map(c => ({ ...c, side: c.export === 'io' ? 'client' : 'server' }));
    assert.deepStrictEqual(verdicts(await verify(checks)), [
      'verified',
      'verified',
      'verified',
      'verified',
      'failed reserved_not_declared',
      'verified',
      'verified',
      'verified',
    ]);
  });

  // --------------------------------------------------------------------------
  // Must not verify, every role
  // --------------------------------------------------------------------------

  it('refuses emit and on a class only inherits from the runtime event emitter', async () => {
    const B = 'fixture-bus';
    const emit = op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } });
    const on = op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 } });
    const checks = [
      check(B, 'Bus', 'socket', 'export', makes('new', null)),
      check(B, 'Bus', 'socket', 'instance:new', emit),
      check(B, 'Bus', 'socket', 'instance:new', on),
      // Its own member verifies.
      check(B, 'Bus', 'socket', 'instance:new', op('send', 'publishLocal', { name: { arg: 0 }, payload: { arg: 1 } })),
      // A maker that returns the runtime's emitter itself owns none of it.
      check(B, 'createBus', 'socket', 'export', makes('call', null)),
      check(B, 'createBus', 'socket', 'instance:()', emit),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), [
      'verified',
      'failed member_inherited',
      'failed member_inherited',
      'verified',
      'verified',
      'failed member_inherited',
    ]);
  });

  it("refuses a member only inherited from another package's base class", async () => {
    const D = 'fixture-derived';
    const checks = [
      check(D, 'Relay', 'broker', 'export', makes('new', null)),
      check(D, 'Relay', 'broker', 'instance:new', op('send', 'send', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(D, 'Relay', 'broker', 'instance:new', op('send', 'forward', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['verified', 'failed member_inherited', 'verified']);
  });

  it("refuses a member only the service's own augmentation adds", async () => {
    const K = 'fixture-kv';
    const checks = [
      check(K, 'createClient', 'broker', 'export', makes('call', null)),
      check(K, 'createClient', 'broker', 'instance:()', op('send', 'broadcast', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['verified', 'failed member_missing']);
  });

  it('refuses an export typed any or unknown, and a bare declare module', async () => {
    const send = op('send', 'publish', { name: { arg: 0 }, payload: { arg: 1 } });
    const checks = [
      check('fixture-any-bus', 'default', 'broker', 'export', send),
      check('fixture-any-bus', 'vague', 'broker', 'export', send),
      check('fixture-shorthand-bus', 'default', 'broker', 'export', send),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), [
      'unchecked export_untyped',
      'unchecked export_untyped',
      'unchecked module_local',
    ]);
  });

  it('refuses a name slot typed as a key of an index-signature map', async () => {
    const R = 'fixture-rules';
    const checks = [
      check(R, 'channel', 'broker', 'export', op('send', 'send', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(R, 'channel', 'broker', 'export', op('send', 'emitKey', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(R, 'channel', 'broker', 'export', op('send', 'sendKey', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), [
      'verified',
      'failed name_index_key',
      'failed name_index_key',
    ]);
  });

  it('refuses a handler typed Function, any, unknown or (...args: any[]), and an options object as a handler', async () => {
    const R = 'fixture-rules';
    const on = (member: string) => check(R, 'channel', 'broker', 'export', op('receive', member, { name: { arg: 0 }, handler: { arg: 1 } }));
    const checks = ['onTyped', 'onAny', 'onUnknown', 'onFunction', 'onRest', 'onOptions'].map(on);
    assert.deepStrictEqual(verdicts(await verify(checks)), [
      'verified',
      'unchecked handler_untyped',
      'unchecked handler_untyped',
      'unchecked handler_untyped',
      'unchecked handler_untyped',
      'failed handler_not_function',
    ]);
  });

  it('refuses a maker that returns any or an unconstrained generic', async () => {
    const R = 'fixture-rules';
    const checks = [
      check(R, 'makeChannel', 'broker', 'export', makes('call', null)),
      check(R, 'makeAny', 'broker', 'export', makes('call', null)),
      check(R, 'makeOpen', 'broker', 'export', makes('call', null)),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), [
      'verified',
      'unchecked maker_unresolved',
      'unchecked maker_unresolved',
    ]);
  });

  it('refuses keys split across union members', async () => {
    const R = 'fixture-rules';
    const define = (member: string) =>
      check(R, 'channel', 'broker', 'export', makes('call', member, { name: { arg: 0, key: 'id' }, handler: { arg: 0, key: 'run' } }));
    assert.deepStrictEqual(verdicts(await verify([define('define'), define('defineSplit')])), [
      'verified',
      'failed key_missing',
    ]);
  });

  it('refuses a name slot beside a string slot the claim leaves unassigned (D2)', async () => {
    const R = 'fixture-rules';
    const on = (member: string, parts: Record<string, unknown>) => check(R, 'channel', 'broker', 'export', op('send', member, parts));
    const checks = [
      // trigger(channel, event, data) with the event as the name: which is the name is behaviour.
      on('trigger', { name: { arg: 1 }, payload: { arg: 2 } }),
      // A string sibling the claim assigns as the payload does not count.
      on('publish', { name: { arg: 0 }, payload: { arg: 1 } }),
      // An unassigned optional string does.
      on('publishWithMeta', { name: { arg: 0 }, payload: { arg: 1 } }),
      // A rest the claim puts the payload in is the payload's.
      on('publishMany', { name: { arg: 0 }, payload: { arg: 1 } }),
      // A rest that holds the name holds other names.
      check(R, 'channel', 'broker', 'export', op('receive', 'subscribeAll', { name: { arg: 0 } })),
      // The options bag with two string keys: the definition's id beside an unassigned description.
      check(R, 'channel', 'broker', 'export', makes('call', 'defineDescribed', { name: { arg: 0, key: 'id' }, handler: { arg: 0, key: 'run' } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), [
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'failed name_ambiguous',
    ]);
  });

  it('reads a name slot typed by a conditional through its branches, unless a branch says nothing', async () => {
    const R = 'fixture-rules';
    const send = (member: string) => check(R, 'dispatcher', 'broker', 'export', op('send', member, { name: { arg: 0 }, payload: { arg: 1 } }));
    assert.deepStrictEqual(verdicts(await verify([send('triggerById'), send('triggerLoose')])), [
      'verified',
      'unchecked member_untyped',
    ]);
  });

  it('refuses a name inside a variadic tuple rest, whose positions are not fixed', async () => {
    const R = 'fixture-rules';
    const checks = [
      check(R, 'dispatcher', 'broker', 'export', op('receive', 'subscribeMany', { name: { arg: 0 }, handler: { arg: 1 } })),
      check(R, 'dispatcher', 'broker', 'export', op('receive', 'subscribeMany', { name: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['failed name_ambiguous', 'failed name_ambiguous']);
  });

  it('refuses a send with no payload slot, and a payload slot that is a callback', async () => {
    const R = 'fixture-rules';
    const checks = [
      // send(data) read as a name alone: the data is the message, not a name.
      check(R, 'dispatcher', 'broker', 'export', op('send', 'sendRaw', { name: { arg: 0 } })),
      check(R, 'dispatcher', 'broker', 'export', op('send', 'sendRaw', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(R, 'dispatcher', 'broker', 'export', op('send', 'ping', { name: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), [
      'unchecked claim_invalid',
      'failed payload_is_function',
      'unchecked claim_invalid',
    ]);
  });

  it('refuses a one-argument send read as both name and payload', async () => {
    const R = 'fixture-rules';
    const checks = [
      check(R, 'channel', 'broker', 'export', op('send', 'push', { name: { arg: 0 }, payload: { arg: 0 } })),
      check(R, 'channel', 'broker', 'export', op('send', 'push', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['failed slots_overlap', 'failed payload_missing']);
  });

  it('refuses a workspace or file: dependency for message roles, and leaves HTTP as #1564 reads it', async () => {
    const L = 'fixture-local-bus';
    const checks = [
      check(L, 'publish', 'broker', 'export', op('send', null, { name: { arg: 0 }, payload: { arg: 1 } })),
      check(L, 'publish', 'http_client', 'export', op('request', null, { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    // Two roles for one export would conflict; ask each in its own request.
    assert.deepStrictEqual(verdicts(await verify([checks[0]])), ['unchecked module_workspace']);
    assert.notStrictEqual(verdicts(await verify([checks[1]]))[0], 'unchecked module_workspace');
  });

  it('gives no fact from an export given two roles in one request', async () => {
    const K = 'fixture-kv';
    const checks = [
      check(K, 'createClient', 'broker', 'export', makes('call', null)),
      check(K, 'createClient', 'socket', 'export', makes('call', null)),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['unchecked role_conflict', 'unchecked role_conflict']);
  });

  it('answers role_unsupported for roles and ops the slice does not read', async () => {
    const K = 'fixture-kv';
    const checks = [
      check(K, 'createClient', 'graphql_client', 'export', makes('call', null)),
      check('@fixture/tasks', 'tasks', 'broker', 'export', op('mount', 'trigger', { name: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await verify(checks)), ['unchecked role_unsupported', 'unchecked role_unsupported']);
  });

  it('answers one result per check, in request order, and rejects a malformed receiver on its own', async () => {
    const T = '@fixture/tasks';
    const checks = [
      check(T, 'tasks', 'broker', 'instance:', op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(T, 'tasks', 'broker', 'export>scope:', op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(T, 'tasks', 'broker', 'export', op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    const response = await verify(checks);
    assert.deepStrictEqual(
      response.semantics!.map(r => `${r.claim_id} @ ${r.receiver}`),
      checks.map(c => `${c.claim_id} @ ${c.receiver}`)
    );
    assert.deepStrictEqual(verdicts(response), ['unchecked receiver_invalid', 'unchecked receiver_invalid', 'verified']);
  });

  // --------------------------------------------------------------------------
  // Readings a slice run can switch on to measure (never the default)
  // --------------------------------------------------------------------------

  it('d2_required_siblings: only a required string sibling makes a name ambiguous', async () => {
    const R = 'fixture-rules';
    const checks = [
      check(R, 'channel', 'broker', 'export', makes('call', 'defineDescribed', { name: { arg: 0, key: 'id' }, handler: { arg: 0, key: 'run' } })),
      check(R, 'channel', 'broker', 'export', op('send', 'publishWithMeta', { name: { arg: 0 }, payload: { arg: 1 } })),
      // A required sibling still competes, and so does a rest that holds the name.
      check(R, 'channel', 'broker', 'export', op('send', 'trigger', { name: { arg: 1 }, payload: { arg: 2 } })),
      check(R, 'channel', 'broker', 'export', op('receive', 'subscribeAll', { name: { arg: 0 } })),
      check(R, 'dispatcher', 'broker', 'export', op('receive', 'subscribeMany', { name: { arg: 0 }, handler: { arg: 1 } })),
    ];
    const strict = ['failed name_ambiguous', 'failed name_ambiguous', 'failed name_ambiguous', 'failed name_ambiguous', 'failed name_ambiguous'];
    assert.deepStrictEqual(verdicts(await verify(checks)), strict);
    assert.deepStrictEqual(verdicts(await verify(checks, ['d2_required_siblings'])), [
      'verified',
      'verified',
      'failed name_ambiguous',
      'failed name_ambiguous',
      'failed name_ambiguous',
    ]);
  });
});
