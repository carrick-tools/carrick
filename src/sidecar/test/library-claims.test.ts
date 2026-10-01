/**
 * carrick#1659: `verify_library_claims` checks claims of the message roles
 * (`broker`, `in_process_bus`, `socket`) against the package's own
 * declarations, in the shape the contract pins on carrick#1564 (comment
 * 5937606126, sections 2 and 3). HTTP parity is pinned in
 * `library-claims-http-parity.test.ts`.
 *
 * Every package below is invented and its declarations are hand-written. The
 * three answered packages are a task SDK (`broker`: a definition maker whose
 * options carry the name and the handler, instance sends with the name bound
 * by the maker, export sends with the name at argument 0), a key-value
 * store's pub/sub client (`broker`, its client re-exported from a core
 * package) and a socket package (`socket`: a client maker and a server
 * class). The rest isolate one must-not-verify rule each, so deleting a rule
 * flips its own fixture.
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
// Two overloads, each with a string key the other lacks.
export declare function pipeline(options: { id: string; run: (payload: unknown) => Promise<unknown>; queue: string }): Task<unknown>;
export declare function pipeline(options: { id: string; run: (payload: unknown) => Promise<unknown>; tag: string }): ScheduledTask;
// An op written the same way on the export and on the instances its makers build.
export interface Topic {
  publish(topic: string, payload: unknown): Promise<void>;
}
export declare const broker: {
  publish(topic: string, payload: unknown): Promise<void>;
  connect(options: { url: string }): Topic;
  reconnect(options: { url: string }): Topic;
};
// A maker one level below the export.
export declare const schedules: {
  task(options: { id: string; run: (payload: unknown) => Promise<unknown>; cron: number }): ScheduledTask;
};
`;

// A subpath of the task SDK, imported as \`@fixture/tasks/v2\`.
const TASKS_V2 = `export declare const tasks: {
  trigger(id: string, payload: unknown): Promise<void>;
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
export declare function io(url: string, options?: { reconnect?: boolean }): ClientSocket;
export declare class Server {
  constructor(options?: { port?: number });
  on(event: 'connection', listener: (socket: ServerSocket) => void): this;
  emit(event: string, payload?: unknown): boolean;
}
`;

// A broker with scope members: a topic bound to its name, directly and one hop down.
const PUBSUB = `export interface Topic {
  publish(message: unknown): Promise<void>;
  subscribe(handler: (message: unknown) => void): void;
}
export interface PubSub {
  topic(name: string): Topic;
  topicLoose(name: string): any;
  admin: { topic(name: string): Topic };
}
export declare function connect(url: string): PubSub;
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
export declare class Holder<F> {
  send: F;
}
export declare class Container {
  inner: { send(topic: string, payload: unknown): void };
}
// A member declared here, typed by a signature the derived package writes.
export declare class Hook {
  send: import('fixture-derived').OwnSend;
}
`;
const DERIVED = `import { Emitter, Holder, Container, Hook } from '@fixture/base-emitter';
export declare class Relay extends Emitter {
  forward(topic: string, payload: unknown): void;
}
// The member is the base's, bound with a signature written here.
export declare class GenericRelay extends Holder<(topic: string, payload: unknown) => void> {}
// The member is the base's, unbound; only its signature is written here.
export type OwnSend = (topic: string, payload: unknown) => void;
export declare class HookRelay extends Hook {}
// The member is written here; its signature is the base's.
export declare class Proxy {
  relay: Emitter['send'];
}
// A sub-object only another package's base declares.
export declare class Box extends Container {}
`;

// A hub whose ops sit one member down: its own, another package's, the runtime's.
const HUB_QUEUE = `export interface Queue {
  add(name: string, data: unknown): Promise<void>;
}
`;
const HUB = `/// <reference types="node" />
import EventEmitter = require('events');
import { Queue } from '@fixture/hub-queue';
export interface Tasks {
  trigger(id: string, payload: unknown): Promise<void>;
}
export interface Hub {
  tasks: Tasks;
  queue: Queue;
  events: EventEmitter;
  loose: any;
  maybe?: Tasks;
}
export declare const hub: Hub;
`;

// A typed emitter package, and a socket package whose classes extend it:
// bound with its own reserved-event map, bound only through its own type
// parameters, and a plain base bound with its own map.
const TYPED_EMITTER = `export interface EventsMap {
  [event: string]: any;
}
export interface DefaultEventsMap {
  [event: string]: (...args: any[]) => void;
}
export declare class Emitter<Listen extends EventsMap, Emit extends EventsMap, Reserved extends EventsMap = {}> {
  on<Ev extends keyof Listen | keyof Reserved>(ev: Ev, listener: (payload: unknown) => void): this;
  emitReserved<Ev extends keyof Reserved>(ev: Ev, payload?: unknown): boolean;
}
export declare class Plain<Reserved extends EventsMap = {}> {
  on(ev: string, listener: (payload: unknown) => void): this;
}
`;
const TYPED_SOCKET = `import { Emitter, DefaultEventsMap, EventsMap, Plain } from '@fixture/typed-emitter';
interface ReservedEvents {
  connect: () => void;
  disconnect: (reason: string) => void;
}
export declare class Socket<Listen extends EventsMap = DefaultEventsMap, Emit extends EventsMap = Listen> extends Emitter<Listen, Emit, ReservedEvents> {
  emit<Ev extends keyof Emit>(ev: Ev, payload?: unknown): this;
}
export declare class PassThrough<Listen extends EventsMap = DefaultEventsMap> extends Emitter<Listen, Listen> {}
export declare class Wire extends Plain<ReservedEvents> {}
export declare function io(url: string): Socket;
export declare function passThrough(): PassThrough;
export declare function wire(): Wire;
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
  publishWithOptions(topic: string, message: unknown, options?: { messageId?: string; ttl?: number }): void;
  deliver(topic: string, envelope: { data: unknown; contentType?: string }): void;
  post(message: { topic: string; body: unknown; replyTo?: string }): void;
  subscribeAll(...topics: string[]): void;
  push(data: string): void;
  onAny(topic: string, handler: any): void;
  onUnknown(topic: string, handler: unknown): void;
  onFunction(topic: string, handler: Function): void;
  onRest(topic: string, handler: (...args: any[]) => void): void;
  onOptions(topic: string, options: { retry?: number }): void;
  onTyped(topic: string, handler: Handler): void;
  onDeferred<K extends string>(topic: K, handler: K extends 'error' ? (error: Error) => void : any): void;
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
export declare const empty: {};
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
type Claim = Record<string, unknown> & { kind: string };
interface Check {
  claim_id: string;
  package: string;
  export: string;
  role: string;
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
  verdicts?: Result[];
  modules?: Array<Record<string, unknown>>;
  duration_ms?: number;
  errors?: string[];
}

let sequence = 0;
function check(pkg: string, exported: string, role: string, receiver: string, claim: Claim): Check {
  return { claim_id: `c${sequence++}`, package: pkg, export: exported, role, receiver, claim };
}
const make = (form: 'call' | 'new', member: string | null, parts: Record<string, unknown> = {}): Claim => ({
  kind: 'make',
  form,
  member,
  ...parts,
});
const op = (opKind: string, member: string | null, parts: Record<string, unknown>): Claim => ({
  kind: 'op',
  op: opKind,
  member,
  ...parts,
});
const scope = (member: string, name: Slot, parts: Record<string, unknown> = {}): Claim => ({
  kind: 'scope',
  member,
  name,
  ...parts,
});
const reserved = (member: string, name: string, parts: Record<string, unknown> = {}): Claim => ({
  kind: 'reserved',
  member,
  name,
  ...parts,
});

function verdicts(response: Response): string[] {
  assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
  return response.verdicts!.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
}

describe('verify_library_claims: message roles (carrick#1659)', () => {
  let root: string;
  let client: SidecarClient;
  let requestId = 0;

  const send = (checks: Check[], variants?: string[]) =>
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
      'node_modules/@fixture/tasks/v2/index.d.ts': TASKS_V2,
      'node_modules/@fixture/kv-core/package.json': packageJson('@fixture/kv-core', '5.1.0', { types: 'index.d.ts' }),
      'node_modules/@fixture/kv-core/index.d.ts': KV_CORE,
      'node_modules/fixture-kv/package.json': packageJson('fixture-kv', '5.1.0', { types: 'index.d.ts' }),
      'node_modules/fixture-kv/index.d.ts': "export * from '@fixture/kv-core';\n",
      'node_modules/fixture-socket/package.json': packageJson('fixture-socket', '2.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-socket/index.d.ts': SOCKET,
      'node_modules/fixture-pubsub/package.json': packageJson('fixture-pubsub', '1.2.0', { types: 'index.d.ts' }),
      'node_modules/fixture-pubsub/index.d.ts': PUBSUB,
      'node_modules/@types/node/package.json': packageJson('@types/node', '22.0.0', { types: 'index.d.ts' }),
      'node_modules/@types/node/index.d.ts': RUNTIME_EVENTS,
      'node_modules/fixture-bus/package.json': packageJson('fixture-bus', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-bus/index.d.ts': BUS,
      'node_modules/@fixture/base-emitter/package.json': packageJson('@fixture/base-emitter', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/@fixture/base-emitter/index.d.ts': BASE_EMITTER,
      'node_modules/fixture-derived/package.json': packageJson('fixture-derived', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-derived/index.d.ts': DERIVED,
      'node_modules/@fixture/hub-queue/package.json': packageJson('@fixture/hub-queue', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/@fixture/hub-queue/index.d.ts': HUB_QUEUE,
      'node_modules/fixture-hub/package.json': packageJson('fixture-hub', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-hub/index.d.ts': HUB,
      'node_modules/@fixture/typed-emitter/package.json': packageJson('@fixture/typed-emitter', '3.0.0', { types: 'index.d.ts' }),
      'node_modules/@fixture/typed-emitter/index.d.ts': TYPED_EMITTER,
      'node_modules/fixture-typed-socket/package.json': packageJson('fixture-typed-socket', '4.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-typed-socket/index.d.ts': TYPED_SOCKET,
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
  // The answered packages
  // --------------------------------------------------------------------------

  it('verifies the task SDK: the definition maker, sends bound to its name, and export sends', async () => {
    const T = '@fixture/tasks';
    const checks = [
      check(T, 'task', 'broker', 'export', make('call', null, { name_key: 'id', handler_key: 'run' })),
      check(T, 'task', 'broker', 'instance:()', op('send', 'trigger', { on: 'instance', name: { bound: 'maker' }, payload: { arg: 0 } })),
      check(T, 'tasks', 'broker', 'export', op('send', 'trigger', { on: 'export', name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    const response = await send(checks);
    assert.deepStrictEqual(verdicts(response), ['verified', 'verified', 'verified']);
    assert.ok(Number.isInteger(response.duration_ms) && response.duration_ms! >= 0, `duration_ms ${response.duration_ms}`);
    assert.deepStrictEqual(response.modules, [
      { package: T, resolved_file: response.modules![0].resolved_file, installed_version: '4.0.0' },
    ]);
  });

  it('takes a null budget as the default, as the contract sample sends it', async () => {
    const response = await client.send<Response>(
      {
        request_id: `claims-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: root,
        budget_ms: null,
        checks: [check('@fixture/tasks', 'tasks', 'broker', 'export', op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 } }))],
      },
      60_000
    );
    assert.deepStrictEqual(verdicts(response), ['verified']);
  });

  it('carries picker and name_scope without reading them', async () => {
    const T = '@fixture/tasks';
    const checks = [
      check(T, 'task', 'broker', 'export', make('call', null, {
        name_key: 'id',
        handler_key: 'run',
        key_labels: { id: 'name' },
        name_scope: { scope: 'service', namespace: 'task' },
        picker: 'model/q1',
      })),
      check(T, 'tasks', 'broker', 'export', op('send', 'trigger', {
        on: 'export',
        name: { arg: 0 },
        payload: { arg: 1 },
        name_scope: { scope: 'service', namespace: null },
        picker: 'model/q1',
      })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'verified']);
  });

  it('reads a subpath specifier as the named package', async () => {
    const checks = [
      check('@fixture/tasks/v2', 'tasks', 'broker', 'export', op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    const response = await send(checks);
    assert.deepStrictEqual(verdicts(response), ['verified']);
    assert.strictEqual(response.modules![0].package, '@fixture/tasks/v2');
    assert.strictEqual(response.modules![0].installed_version, '4.0.0');
  });

  it('reads an instance only through a maker claim that holds in the same request', async () => {
    const T = '@fixture/tasks';
    const sendBound = op('send', 'trigger', { name: { bound: 'maker' }, payload: { arg: 0 } });
    assert.deepStrictEqual(verdicts(await send([check(T, 'task', 'broker', 'instance:()', sendBound)])), [
      'unchecked maker_unverified',
    ]);
    // A maker claim that does not hold builds no instance.
    const wrongMaker = make('call', null, { name_key: 'name' });
    assert.deepStrictEqual(
      verdicts(await send([check(T, 'task', 'broker', 'export', wrongMaker), check(T, 'task', 'broker', 'instance:()', sendBound)])),
      ['failed key_missing', 'unchecked maker_unverified']
    );
    // A name bound by a maker that binds none.
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(T, 'task', 'broker', 'export', make('call', null, { handler_key: 'run' })),
          check(T, 'task', 'broker', 'instance:()', sendBound),
        ])
      ),
      ['verified', 'failed name_unbound']
    );
  });

  it('reads an instance through every maker overload that holds, and needs the op on each', async () => {
    const T = '@fixture/tasks';
    const maker = make('call', null, { name_key: 'id', handler_key: 'run' });
    const checks = [
      check(T, 'job', 'broker', 'export', maker),
      check(T, 'job', 'broker', 'instance:()', op('send', 'trigger', { name: { bound: 'maker' }, payload: { arg: 0 } })),
      // Only one overload's instance can cancel.
      check(T, 'job', 'broker', 'instance:()', op('send', 'cancel', { name: { bound: 'maker' }, payload: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'verified', 'failed member_missing']);
  });

  it('reads an instance through the overloads every maker claim of the request holds on', async () => {
    const T = '@fixture/tasks';
    const definition = make('call', null, {
      name_key: 'id',
      handler_key: 'run',
      key_labels: { queue: 'not_name', tag: 'not_name' },
    });
    const tagged = make('call', null, { prefix_key: 'tag' });
    const queued = make('call', null, { prefix_key: 'queue' });
    const cancel = op('receive', 'cancel', { name: { bound: 'maker' } });
    // Only the second overload takes a tag, and only its instance can cancel.
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(T, 'pipeline', 'broker', 'export', definition),
          check(T, 'pipeline', 'broker', 'export', tagged),
          check(T, 'pipeline', 'broker', 'instance:()', cancel),
        ])
      ),
      ['verified', 'verified', 'verified']
    );
    assert.deepStrictEqual(
      verdicts(await send([check(T, 'pipeline', 'broker', 'export', definition), check(T, 'pipeline', 'broker', 'instance:()', cancel)])),
      ['verified', 'failed member_missing']
    );
    // No overload takes both keys: no instance is made.
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(T, 'pipeline', 'broker', 'export', tagged),
          check(T, 'pipeline', 'broker', 'export', queued),
          check(T, 'pipeline', 'broker', 'instance:()', cancel),
        ])
      ),
      ['verified', 'verified', 'unchecked maker_unresolved']
    );
  });

  it('verifies the key-value client re-exported from its core package', async () => {
    const K = 'fixture-kv';
    const checks = [
      check(K, 'createClient', 'broker', 'export', make('call', null, { base_key: 'url' })),
      check(K, 'createClient', 'broker', 'instance:()', op('send', 'publish', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(K, 'createClient', 'broker', 'instance:()', op('receive', 'subscribe', { name: { arg: 0 }, handler: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'verified', 'verified']);
  });

  it('verifies the socket client and server, and the names they declare for themselves', async () => {
    const S = 'fixture-socket';
    const checks = [
      check(S, 'io', 'socket', 'export', make('call', null)),
      check(S, 'io', 'socket', 'instance:()', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 }, ack: { arg: 2 } })),
      check(S, 'io', 'socket', 'instance:()', op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 } })),
      check(S, 'io', 'socket', 'instance:()', reserved('on', 'connect', { on: 'instance' })),
      check(S, 'io', 'socket', 'instance:()', reserved('on', 'reconnect', { on: 'instance' })),
      check(S, 'Server', 'socket', 'export', make('new', null)),
      check(S, 'Server', 'socket', 'instance:new', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(S, 'Server', 'socket', 'instance:new', reserved('on', 'connection')),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
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
  // Receivers: on, of, makers on the export, scopes
  // --------------------------------------------------------------------------

  it('reads an op, scope or reserved name only on the receivers its on and of name', async () => {
    const T = '@fixture/tasks';
    const definition = make('call', null, { name_key: 'id', handler_key: 'run' });
    const scheduled = make('call', 'task', { name_key: 'id', handler_key: 'run' });
    const bound = (parts: Record<string, unknown>) =>
      op('send', 'trigger', { name: { bound: 'maker' }, payload: { arg: 0 }, ...parts });
    const exportSend = (parts: Record<string, unknown>) =>
      op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 }, ...parts });
    const publishBoth = op('send', 'publish', { name: { arg: 0 }, payload: { arg: 1 }, on: 'both', of: 'connect' });
    const checks = [
      check(T, 'task', 'broker', 'export', definition),
      check(T, 'schedules', 'broker', 'export', scheduled),
      // Claimed for instances, asked on the export.
      check(T, 'tasks', 'broker', 'export', exportSend({ on: 'instance' })),
      // Claimed for the export, asked on an instance.
      check(T, 'task', 'broker', 'instance:()', bound({ on: 'export' })),
      // Claimed of one maker, asked on another's instance.
      check(T, 'schedules', 'broker', 'instance:task', bound({ on: 'instance', of: 'cron' })),
      check(T, 'task', 'broker', 'instance:()', bound({ on: 'instance', of: 'task' })),
      // Claimed of the maker it is asked on.
      check(T, 'schedules', 'broker', 'instance:task', bound({ on: 'instance', of: 'task' })),
      check(T, 'tasks', 'broker', 'export', exportSend({ on: 'both' })),
      check(T, 'task', 'broker', 'instance:()', bound({ on: 'both' })),
      // A reserved name claimed for the export, asked on an instance.
      check(T, 'task', 'broker', 'instance:()', reserved('trigger', 'x', { on: 'export' })),
      // Claimed on the export and on one maker's instances: of says nothing about the export leg.
      check(T, 'broker', 'broker', 'export', make('call', 'connect', { base_key: 'url' })),
      check(T, 'broker', 'broker', 'export', make('call', 'reconnect', { base_key: 'url' })),
      check(T, 'broker', 'broker', 'export', publishBoth),
      check(T, 'broker', 'broker', 'instance:connect', publishBoth),
      check(T, 'broker', 'broker', 'instance:reconnect', publishBoth),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'verified',
      'unchecked receiver_invalid',
      'unchecked receiver_invalid',
      'unchecked receiver_invalid',
      'unchecked receiver_invalid',
      'verified',
      'verified',
      'verified',
      'unchecked receiver_invalid',
      'verified',
      'verified',
      'verified',
      'verified',
      'unchecked receiver_invalid',
    ]);
  });

  it('reads a maker on the export only', async () => {
    const T = '@fixture/tasks';
    const definition = make('call', null, { name_key: 'id', handler_key: 'run' });
    const checks = [
      check(T, 'task', 'broker', 'export', definition),
      check(T, 'task', 'broker', 'instance:()', definition),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'unchecked receiver_invalid']);
  });

  it('reads ops on a scope only through a scope claim that holds in the same request', async () => {
    const P = 'fixture-pubsub';
    const connect = make('call', null);
    const topic = scope('topic', { arg: 0 }, { on: 'instance' });
    const publish = op('send', 'publish', { name: { bound: 'scope' }, payload: { arg: 0 } });
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(P, 'connect', 'broker', 'export', connect),
          check(P, 'connect', 'broker', 'instance:()', topic),
          check(P, 'connect', 'broker', 'instance:()>scope:topic', publish),
          check(P, 'connect', 'broker', 'instance:()>scope:topic', op('receive', 'subscribe', { name: { bound: 'scope' }, handler: { arg: 0 } })),
          // A scope one member down, named by its path.
          check(P, 'connect', 'broker', 'instance:()', scope('topic', { arg: 0 }, { path: ['admin'] })),
          check(P, 'connect', 'broker', 'instance:()>scope:admin.topic', publish),
          // A name bound by a scope on a receiver no scope made.
          check(P, 'connect', 'broker', 'instance:()', publish),
        ])
      ),
      ['verified', 'verified', 'verified', 'verified', 'verified', 'verified', 'failed name_unbound']
    );
    assert.deepStrictEqual(
      verdicts(await send([check(P, 'connect', 'broker', 'export', connect), check(P, 'connect', 'broker', 'instance:()>scope:topic', publish)])),
      ['verified', 'unchecked scope_unverified']
    );
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(P, 'connect', 'broker', 'export', connect),
          check(P, 'connect', 'broker', 'instance:()', scope('topicLoose', { arg: 0 })),
          check(P, 'connect', 'broker', 'instance:()>scope:topicLoose', publish),
        ])
      ),
      ['verified', 'unchecked scope_unresolved', 'unchecked scope_unverified']
    );
  });

  it('walks a member path one home member at a time', async () => {
    const H = 'fixture-hub';
    const at = (hops: string[], member: string) =>
      check(H, 'hub', 'broker', 'export', op('send', member, { path: hops, name: { arg: 0 }, payload: { arg: 1 } }));
    const checks = [
      at(['tasks'], 'trigger'),
      // An optional sub-object is read without its undefined.
      at(['maybe'], 'trigger'),
      // A sub-object another package types: that package declares its members.
      at(['queue'], 'add'),
      // The runtime's emitter as a sub-object: its members stay the runtime's.
      at(['events'], 'emit'),
      // A sub-object typed any has no members to read.
      at(['loose'], 'publish'),
      at(['absent'], 'trigger'),
      // Without the path the member is not there.
      at([], 'trigger'),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'verified',
      'verified',
      'failed member_inherited',
      'unchecked member_untyped',
      'failed member_missing',
      'failed member_missing',
    ]);
    // A sub-object only another package's base declares.
    const D = 'fixture-derived';
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(D, 'Box', 'broker', 'export', make('new', null)),
          check(D, 'Box', 'broker', 'instance:new', op('send', 'send', { path: ['inner'], name: { arg: 0 }, payload: { arg: 1 } })),
        ])
      ),
      ['verified', 'failed member_inherited']
    );
  });

  // --------------------------------------------------------------------------
  // Must not verify, every message role
  // --------------------------------------------------------------------------

  it('refuses emit and on a class only inherits from the runtime event emitter', async () => {
    const B = 'fixture-bus';
    const emit = op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } });
    const on = op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 } });
    const checks = [
      check(B, 'Bus', 'socket', 'export', make('new', null)),
      check(B, 'Bus', 'socket', 'instance:new', emit),
      check(B, 'Bus', 'socket', 'instance:new', on),
      // Its own member verifies.
      check(B, 'Bus', 'socket', 'instance:new', op('send', 'publishLocal', { name: { arg: 0 }, payload: { arg: 1 } })),
      // A maker that returns the runtime's emitter itself owns none of it.
      check(B, 'createBus', 'socket', 'export', make('call', null)),
      check(B, 'createBus', 'socket', 'instance:()', emit),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
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
    const sendOn = (member: string) => op('send', member, { name: { arg: 0 }, payload: { arg: 1 } });
    const checks = [
      check(D, 'Relay', 'broker', 'export', make('new', null)),
      check(D, 'Relay', 'broker', 'instance:new', sendOn('send')),
      check(D, 'Relay', 'broker', 'instance:new', sendOn('forward')),
      check(D, 'HookRelay', 'broker', 'export', make('new', null)),
      check(D, 'HookRelay', 'broker', 'instance:new', sendOn('send')),
      check(D, 'Proxy', 'broker', 'export', make('new', null)),
      check(D, 'Proxy', 'broker', 'instance:new', sendOn('relay')),
      // A base the class binds with its own type: the member counts (see the bound-emitter test).
      check(D, 'GenericRelay', 'broker', 'export', make('new', null)),
      check(D, 'GenericRelay', 'broker', 'instance:new', sendOn('send')),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'failed member_inherited',
      'verified',
      'verified',
      // Stopped by the member rule alone: the signature is written here.
      'failed member_inherited',
      'verified',
      // Stopped by the signature rule alone: the member is written here.
      'failed member_inherited',
      'verified',
      'verified',
    ]);
  });

  it('counts an inherited emitter the client binds to its own declared interface', async () => {
    const S = 'fixture-typed-socket';
    const on = op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 } });
    const checks = [
      // Bound with the package's own reserved-event map: the member counts.
      check(S, 'wire', 'socket', 'export', make('call', null)),
      check(S, 'wire', 'socket', 'instance:()', on),
      // Bound only through its own type parameters: still another package's member.
      check(S, 'passThrough', 'socket', 'export', make('call', null)),
      check(S, 'passThrough', 'socket', 'instance:()', on),
      // The runtime's emitter, extended with nothing of the package's own.
      check('fixture-bus', 'Bus', 'broker', 'export', make('new', null)),
      check('fixture-bus', 'Bus', 'broker', 'instance:new', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'verified',
      'verified',
      'failed member_inherited',
      'verified',
      'failed member_inherited',
    ]);
  });

  it("refuses a member only the service's own augmentation adds", async () => {
    const K = 'fixture-kv';
    const checks = [
      check(K, 'createClient', 'broker', 'export', make('call', null)),
      check(K, 'createClient', 'broker', 'instance:()', op('send', 'broadcast', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'failed member_missing']);
  });

  it('refuses an export typed any or unknown, and a bare declare module', async () => {
    const publish = op('send', 'publish', { name: { arg: 0 }, payload: { arg: 1 } });
    const checks = [
      check('fixture-any-bus', 'default', 'broker', 'export', publish),
      check('fixture-any-bus', 'vague', 'broker', 'export', publish),
      check('fixture-any-bus', 'empty', 'broker', 'export', publish),
      check('fixture-shorthand-bus', 'default', 'broker', 'export', publish),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'unchecked export_untyped',
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
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'failed name_index_key',
      'failed name_index_key',
    ]);
  });

  it('refuses a handler typed Function, any, unknown or (...args: any[]), and an options object as a handler', async () => {
    const R = 'fixture-rules';
    const on = (member: string) => check(R, 'channel', 'broker', 'export', op('receive', member, { name: { arg: 0 }, handler: { arg: 1 } }));
    const checks = ['onTyped', 'onAny', 'onUnknown', 'onFunction', 'onRest', 'onOptions', 'onDeferred'].map(on);
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'unchecked handler_untyped',
      'unchecked handler_untyped',
      'unchecked handler_untyped',
      'unchecked handler_untyped',
      'failed handler_not_function',
      // Conditional machinery with an any branch says nothing.
      'unchecked handler_untyped',
    ]);
  });

  it('refuses a maker that returns any or an unconstrained generic', async () => {
    const R = 'fixture-rules';
    const checks = [
      check(R, 'makeChannel', 'broker', 'export', make('call', null)),
      check(R, 'makeAny', 'broker', 'export', make('call', null)),
      check(R, 'makeOpen', 'broker', 'export', make('call', null)),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'unchecked maker_unresolved',
      'unchecked maker_unresolved',
    ]);
  });

  it('refuses keys split across union members', async () => {
    const R = 'fixture-rules';
    const define = (member: string) =>
      check(R, 'channel', 'broker', 'export', make('call', member, { name_key: 'id', handler_key: 'run' }));
    assert.deepStrictEqual(verdicts(await send([define('define'), define('defineSplit')])), [
      'verified',
      'failed key_missing',
    ]);
  });

  it('refuses a name slot beside a string slot the claim does not account for (strict D2)', async () => {
    const R = 'fixture-rules';
    const at = (claim: Claim) => check(R, 'channel', 'broker', 'export', claim);
    const checks = [
      // trigger(channel, event, data) with the event as the name: which is the name is behaviour.
      at(op('send', 'trigger', { name: { arg: 1 }, payload: { arg: 2 } })),
      // A string sibling the claim assigns as the payload does not count.
      at(op('send', 'publish', { name: { arg: 0 }, payload: { arg: 1 } })),
      // An unassigned optional string does: a positional slot cannot be labelled.
      at(op('send', 'publishWithMeta', { name: { arg: 0 }, payload: { arg: 1 } })),
      // A rest the claim puts the payload in is the payload's.
      at(op('send', 'publishMany', { name: { arg: 0 }, payload: { arg: 1 } })),
      // A rest that holds the name holds other names.
      at(op('receive', 'subscribeAll', { name: { arg: 0 } })),
      // The definition's id beside an optional description nobody labels.
      at(make('call', 'defineDescribed', { name_key: 'id', handler_key: 'run' })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'failed name_ambiguous',
    ]);
  });

  it('accepts a string key labelled not_name, and checks the labels for completeness', async () => {
    const R = 'fixture-rules';
    const define = (labels?: Record<string, string>) =>
      check(R, 'channel', 'broker', 'export', make('call', 'defineDescribed', {
        name_key: 'id',
        handler_key: 'run',
        ...(labels === undefined ? {} : { key_labels: labels }),
      }));
    const checks = [
      define({ id: 'name', description: 'not_name' }),
      // The name key need not be labelled; every sibling must.
      define({ description: 'not_name' }),
      define({ id: 'name' }),
      // The claim's name key labelled not the name.
      define({ id: 'not_name', description: 'not_name' }),
      // Another key labelled the name.
      define({ description: 'name' }),
      define({ id: 'name', description: 'name' }),
      // A key assigned another part may carry a label; a key the type does not declare is ignored.
      define({ id: 'name', description: 'not_name', run: 'not_name', queue: 'not_name' }),
      // Unless it is labelled the name: one map serves every overload, and another overload may declare it.
      define({ description: 'not_name', queue: 'name' }),
      define({ id: 'name', description: 'not_name', queue: 'name' }),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'verified',
      'failed name_ambiguous',
      'failed name_ambiguous',
      'failed name_ambiguous',
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'failed name_ambiguous',
    ]);
  });

  it('counts every string key at the call, not only the keys beside a keyed name', async () => {
    const R = 'fixture-rules';
    const at = (claim: Claim) => check(R, 'channel', 'broker', 'export', claim);
    const checks = [
      // A positional name beside an options bag no part is assigned to.
      at(op('send', 'publishWithOptions', { name: { arg: 0 }, payload: { arg: 1 } })),
      at(op('send', 'publishWithOptions', { name: { arg: 0 }, payload: { arg: 1 }, key_labels: { messageId: 'not_name' } })),
      // A labelled key of that bag cannot be the name when the name is positional.
      at(op('send', 'publishWithOptions', { name: { arg: 0 }, payload: { arg: 1 }, key_labels: { messageId: 'name' } })),
      // The keys beside a keyed payload.
      at(op('send', 'deliver', { name: { arg: 0 }, payload: { arg: 1, key: 'data' } })),
      at(op('send', 'deliver', { name: { arg: 0 }, payload: { arg: 1, key: 'data' }, key_labels: { contentType: 'not_name' } })),
      // A keyed name and a keyed payload in one message object.
      at(op('send', 'post', { name: { arg: 0, key: 'topic' }, payload: { arg: 0, key: 'body' } })),
      at(op('send', 'post', { name: { arg: 0, key: 'topic' }, payload: { arg: 0, key: 'body' }, key_labels: { topic: 'name', replyTo: 'not_name' } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'verified',
    ]);
  });

  it('reads a name slot typed by a conditional through its branches, unless a branch says nothing', async () => {
    const R = 'fixture-rules';
    const at = (member: string) => check(R, 'dispatcher', 'broker', 'export', op('send', member, { name: { arg: 0 }, payload: { arg: 1 } }));
    assert.deepStrictEqual(verdicts(await send([at('triggerById'), at('triggerLoose')])), [
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
    assert.deepStrictEqual(verdicts(await send(checks)), ['failed name_ambiguous', 'failed name_ambiguous']);
  });

  it('refuses a send with no payload slot, and a payload slot that is a callback', async () => {
    const R = 'fixture-rules';
    const checks = [
      // send(data) read as a name alone: the data is the message, not a name.
      check(R, 'dispatcher', 'broker', 'export', op('send', 'sendRaw', { name: { arg: 0 } })),
      check(R, 'dispatcher', 'broker', 'export', op('send', 'sendRaw', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(R, 'dispatcher', 'broker', 'export', op('send', 'ping', { name: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
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
    assert.deepStrictEqual(verdicts(await send(checks)), ['failed slots_overlap', 'failed payload_missing']);
  });

  it('refuses a workspace or file: dependency for message roles, and leaves HTTP as #1564 reads it', async () => {
    const L = 'fixture-local-bus';
    const message = check(L, 'publish', 'broker', 'export', op('send', null, { name: { arg: 0 }, payload: { arg: 1 } }));
    const http = check(L, 'publish', 'http_client', 'export', op('request', null, { name: { arg: 0 }, payload: { arg: 1 } }));
    // Two roles for one export would conflict; ask each in its own request.
    assert.deepStrictEqual(verdicts(await send([message])), ['unchecked module_workspace']);
    assert.notStrictEqual(verdicts(await send([http]))[0], 'unchecked module_workspace');
  });

  it('reads an HTTP claim only in the parts its #1564 kinds use', async () => {
    const K = 'fixture-kv';
    const factory = (parts: Record<string, unknown>) =>
      check(K, 'createClient', 'http_client', 'export', make('call', 'create', { base_key: 'url', ...parts }));
    const checks = [
      factory({ name_key: 'id' }),
      factory({ handler_key: 'run' }),
      factory({ prefix_key: 'prefix' }),
      check(K, 'createClient', 'http_client', 'export', op('request', 'get', { method: 'GET', name: { arg: 0 }, path: ['client'] })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
    ]);
  });

  it('gives no fact from an export given two roles in one request', async () => {
    const K = 'fixture-kv';
    const checks = [
      check(K, 'createClient', 'broker', 'export', make('call', null)),
      check(K, 'createClient', 'socket', 'export', make('call', null)),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['unchecked role_conflict', 'unchecked role_conflict']);
  });

  it('answers role_unsupported for roles and ops this build does not read', async () => {
    const checks = [
      check('fixture-kv', 'createClient', 'graphql_client', 'export', make('call', null)),
      check('fixture-kv', 'get', 'server_framework', 'export', make('call', null)),
      check('@fixture/tasks', 'tasks', 'broker', 'export', op('execute', 'trigger', { name: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'unchecked role_unsupported',
      'unchecked role_unsupported',
      'unchecked role_unsupported',
    ]);
  });

  it('answers one verdict per check, in request order, and rejects a malformed receiver on its own', async () => {
    const T = '@fixture/tasks';
    const trigger = op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 } });
    const checks = [
      check(T, 'tasks', 'broker', 'instance:', trigger),
      check(T, 'tasks', 'broker', 'export>scope:', trigger),
      check(T, 'tasks', 'broker', 'export', trigger),
    ];
    const response = await send(checks);
    assert.deepStrictEqual(
      response.verdicts!.map(r => `${r.claim_id} @ ${r.receiver}`),
      checks.map(c => `${c.claim_id} @ ${c.receiver}`)
    );
    assert.deepStrictEqual(verdicts(response), ['unchecked receiver_invalid', 'unchecked receiver_invalid', 'verified']);
  });

  it('rejects a request whose claim is outside the contract shape', async () => {
    const T = '@fixture/tasks';
    const shapes: Claim[] = [
      // There is no define op: a definition is a maker with a name key and a handler key.
      op('define', 'task', { name: { arg: 0 } }),
      // A maker's keys are named, not placed.
      { kind: 'make', form: 'call', member: null, name: { arg: 0, key: 'id' } },
      { kind: 'reserved', member: 'on', name: 'connect', at: { arg: 0 } },
      make('call', null, { name_key: 'id', key_labels: { id: 'maybe' } }),
    ];
    for (const claim of shapes) {
      const response = await send([check(T, 'task', 'broker', 'export', claim)]);
      assert.strictEqual(response.status, 'error', JSON.stringify(claim));
    }
  });

  // --------------------------------------------------------------------------
  // A reading a request can switch on (never the default)
  // --------------------------------------------------------------------------

  it('index_key_generic_map reads a key of a generic event map as a string slot, only when asked', async () => {
    const S = 'fixture-typed-socket';
    const checks = [
      check(S, 'io', 'socket', 'export', make('call', null)),
      check(S, 'io', 'socket', 'instance:()', op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 } })),
      check(S, 'io', 'socket', 'instance:()', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } })),
      // A key of a concrete index-signature map.
      check('fixture-rules', 'channel', 'broker', 'export', op('send', 'sendKey', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'failed name_index_key',
      'failed name_index_key',
      'failed name_index_key',
    ]);
    assert.deepStrictEqual(verdicts(await send(checks, ['index_key_generic_map'])), [
      'verified',
      'verified',
      'verified',
      'failed name_index_key',
    ]);
  });
});
