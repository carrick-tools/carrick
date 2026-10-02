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

// The runtime's event emitter, as its type package declares it (generic in
// its event map, defaulting to "no map"), under the bare name and the node:
// name.
const RUNTIME_EVENTS = `declare module 'events' {
  type DefaultEventMap = [never];
  type EventMap<T> = Record<keyof T, any[]> | DefaultEventMap;
  type AnyRest = [...args: any[]];
  type Args<K, T> = T extends DefaultEventMap ? AnyRest : K extends keyof T ? T[K] : never;
  type Key<K, T> = T extends DefaultEventMap ? string | symbol : K | keyof T;
  type Listener<K, T, F> = T extends DefaultEventMap ? F : K extends keyof T ? (T[K] extends unknown[] ? (...args: T[K]) => void : never) : never;
  type Listener1<K, T> = Listener<K, T, (...args: any[]) => void>;
  class EventEmitter<T extends EventMap<T> = DefaultEventMap> {
    constructor(options?: { captureRejections?: boolean });
    emit<K>(eventName: Key<K, T>, ...args: Args<K, T>): boolean;
    on<K>(eventName: Key<K, T>, listener: Listener1<K, T>): this;
  }
  export = EventEmitter;
}
declare module 'node:events' {
  import EventEmitter = require('events');
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
  // A task SDK's trigger: the id is typed by a conditional on the task.
  triggerWith<D extends Definition<string>>(id: IdOf<D>, payload: unknown, options?: { concurrencyKey?: string; delay?: number }): void;
  triggerLooseWith<D extends Definition<string>>(id: IdOrAny<D>, payload: unknown, options?: { concurrencyKey?: string }): void;
  watchById<D extends Definition<string>>(id: IdOf<D>, channel: string): void;
  defineTyped<D extends Definition<string>>(options: { id: IdOf<D>; queue?: string; run: Handler }): void;
  subscribeMany(...args: [...channels: string[], callback: (err: Error | null) => void]): void;
  sendRaw(data: string, callback?: (err?: Error) => void): void;
  ping(event: string): void;
}
export declare const channel: Channel;
export declare const dispatcher: Dispatcher;
export declare function makeAny(): any;
export declare function makeOpen<T>(): T;
export declare function makeChannel(): Channel;
export declare function makeDefault<T = Channel>(): T;
`;

// Makers whose parts are positional (contract amendment 2, B2): a queue named
// at argument 0, a worker with its handler at argument 1, a client whose base
// is argument 0.
const QUEUE = `export interface QueueOptions {
  connection?: { host: string; port?: number };
  prefix?: string;
}
export declare class Queue<Data = unknown> {
  constructor(name: string, options?: QueueOptions);
  publish(data: Data): Promise<void>;
}
export declare class Worker {
  constructor(name: string, processor: (job: { data: unknown }) => Promise<void>, options?: { concurrency?: number });
  close(): Promise<void>;
}
export declare class Pair {
  constructor(name: string, group?: string);
  publish(data: unknown): Promise<void>;
}
export interface Connection {
  publish(topic: string, data: unknown): void;
}
export declare function connect(url: string, options?: { reconnect?: boolean }): Connection;
export declare function open(port: number): Connection;
type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2) ? true : false;
export interface Backend { kind: string }
export interface DefaultBackend extends Backend { kind: 'default' }
export type BackendFactory<B> = (name: string) => B;
// What the queue takes at its default backend: a conditional on the backend
// type parameter, so a rest the declared signature cannot read.
export type DefaultRest<B> = true extends Equal<B, DefaultBackend> ? [options?: QueueOptions, backendFactory?: undefined] : [options: never];
export declare class BackendQueue<B extends Backend = DefaultBackend> {
  constructor(name: string, options: QueueOptions, backendFactory: BackendFactory<B>);
  constructor(name: string, options: QueueOptions, backendFactory?: undefined);
  constructor(name: string, ...args: DefaultRest<B>);
  publish(data: unknown): Promise<void>;
}
export interface Plain {}
export type Rest<B> = B extends Plain ? [options?: QueueOptions] : [options: QueueOptions, factory: () => void];
export declare class Spread<B = Plain> {
  constructor(name: string, ...args: Rest<B>);
  close(): void;
}
export declare function emitTo<B = Plain>(name: string, ...args: Rest<B>): void;
`;

// Generic makers and scopes, read at their type-parameter defaults
// (contract amendment 2, B6).
const GENERIC = `type DefaultMap = [never];
type EventMap<T> = Record<keyof T, unknown[]> | DefaultMap;
type Key<K, T> = T extends DefaultMap ? string | symbol : K | keyof T;
export interface Channel<T> {
  emit<K>(eventName: Key<K, T>, data: unknown): boolean;
}
// A scope whose own type parameter defaults to the class's.
export interface Topic {
  publish(data: unknown): void;
}
export declare class Hub<T extends EventMap<T> = DefaultMap> {
  constructor();
  room<R extends EventMap<R> = T>(name: string): Channel<R>;
  // A scope whose return is its own type parameter, with a default.
  topic<P = Topic>(name: string): P;
}
// Two overloads with their own defaults: an instance one builds says nothing
// about the other's.
export interface Line<N> {
  send(name: N, data: unknown): void;
}
export declare function line<N = string>(port: number): Line<N>;
export declare function line<M = number>(options: { id: string }): Line<M>;
`;

// An HTTP client whose factory builds a different instance per options type.
const HTTP_FACTORY = `export interface Plain {
  get(url: string): Promise<unknown>;
}
export interface Prefixed {
  get(url: string): Promise<unknown>;
}
export interface Static {
  create(options: { timeout?: number }): Plain;
  create(options: { prefixUrl: string }): Prefixed;
}
declare const client: Static;
export default client;
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

/** A key of the options object at argument 0. */
const keyed = (key: string): Slot => ({ arg: 0, key });

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

  const send = (checks: Check[]) =>
    client.send<Response>(
      {
        request_id: `claims-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: root,
        checks,
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
      'types/shims.d.ts':
        "declare module 'fixture-shorthand-bus';\ndeclare module 'node:shim' {\n  export function publish(topic: string, payload: unknown): void;\n}\n",
      // The service compiles against the runtime's types, as a real Node service does.
      'src/runtime.ts': '/// <reference types="node" />\nexport {};\n',
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
      'node_modules/fixture-queue/package.json': packageJson('fixture-queue', '3.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-queue/index.d.ts': QUEUE,
      'node_modules/fixture-generic/package.json': packageJson('fixture-generic', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-generic/index.d.ts': GENERIC,
      'node_modules/fixture-http-factory/package.json': packageJson('fixture-http-factory', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-http-factory/index.d.ts': HTTP_FACTORY,
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
      check(T, 'task', 'broker', 'export', make('call', null, { name: keyed('id'), handler: keyed('run') })),
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
        name: keyed('id'),
        handler: keyed('run'),
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
    const wrongMaker = make('call', null, { name: keyed('name') });
    assert.deepStrictEqual(
      verdicts(await send([check(T, 'task', 'broker', 'export', wrongMaker), check(T, 'task', 'broker', 'instance:()', sendBound)])),
      ['failed key_missing', 'unchecked maker_unverified']
    );
    // A name bound by a maker that binds none.
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(T, 'task', 'broker', 'export', make('call', null, { handler: keyed('run') })),
          check(T, 'task', 'broker', 'instance:()', sendBound),
        ])
      ),
      ['verified', 'failed name_unbound']
    );
  });

  it('reads an instance through every maker overload that holds, and needs the op on each', async () => {
    const T = '@fixture/tasks';
    const maker = make('call', null, { name: keyed('id'), handler: keyed('run') });
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
      name: keyed('id'),
      handler: keyed('run'),
      key_labels: { queue: 'not_name', tag: 'not_name' },
    });
    const tagged = make('call', null, { prefix: keyed('tag') });
    const queued = make('call', null, { prefix: keyed('queue') });
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
      check(K, 'createClient', 'broker', 'export', make('call', null, { base: keyed('url') })),
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

  it('reads an op, scope or reserved name only on the receivers its on and of name (amendment 2, B3)', async () => {
    const T = '@fixture/tasks';
    const definition = make('call', null, { name: keyed('id'), handler: keyed('run') });
    const scheduled = make('call', 'task', { name: keyed('id'), handler: keyed('run') });
    const bound = (parts: Record<string, unknown>) =>
      op('send', 'trigger', { name: { bound: 'maker' }, payload: { arg: 0 }, ...parts });
    const exportSend = (parts: Record<string, unknown>) =>
      op('send', 'trigger', { name: { arg: 0 }, payload: { arg: 1 }, ...parts });
    const checks = [
      check(T, 'task', 'broker', 'export', definition),
      check(T, 'schedules', 'broker', 'export', scheduled),
      // Claimed for instances, asked on the export.
      check(T, 'tasks', 'broker', 'export', exportSend({ on: 'instance' })),
      // Claimed for the export, asked on an instance.
      check(T, 'task', 'broker', 'instance:()', bound({ on: 'export' })),
      // `of` names one receiver by its id: asked on another.
      check(T, 'schedules', 'broker', 'instance:task', bound({ of: 'instance:cron' })),
      check(T, 'task', 'broker', 'instance:()', bound({ of: 'instance:task' })),
      // Asked on the receiver it names.
      check(T, 'schedules', 'broker', 'instance:task', bound({ of: 'instance:task' })),
      check(T, 'task', 'broker', 'instance:()', bound({ of: 'instance:()' })),
      check(T, 'tasks', 'broker', 'export', exportSend({ on: 'both' })),
      check(T, 'task', 'broker', 'instance:()', bound({ on: 'both' })),
      // A reserved name claimed for the export, asked on an instance.
      check(T, 'task', 'broker', 'instance:()', reserved('trigger', 'x', { on: 'export' })),
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
      'verified',
      'unchecked receiver_invalid',
    ]);
  });

  it('reads on and of as exclusive, and a member on both receivers as two elements (amendment 2, B3)', async () => {
    const T = '@fixture/tasks';
    const publish = (parts: Record<string, unknown>) =>
      op('send', 'publish', { name: { arg: 0 }, payload: { arg: 1 }, ...parts });
    const checks = [
      check(T, 'broker', 'broker', 'export', make('call', 'connect', { base: keyed('url') })),
      check(T, 'broker', 'broker', 'export', make('call', 'reconnect', { base: keyed('url') })),
      // The export's leg and one maker's leg, each its own element.
      check(T, 'broker', 'broker', 'export', publish({ on: 'export' })),
      check(T, 'broker', 'broker', 'instance:connect', publish({ of: 'instance:connect' })),
      check(T, 'broker', 'broker', 'instance:reconnect', publish({ of: 'instance:connect' })),
      // An element carrying both says no one receiver, on any receiver.
      check(T, 'broker', 'broker', 'export', publish({ on: 'both', of: 'instance:connect' })),
      check(T, 'broker', 'broker', 'instance:connect', publish({ on: 'both', of: 'instance:connect' })),
      check(T, 'broker', 'broker', 'instance:connect', publish({ on: 'instance', of: 'instance:connect' })),
      // `of` is a receiver id a maker or scope builds: a maker's member name is not one, nor is the export.
      check(T, 'broker', 'broker', 'instance:connect', publish({ of: 'connect' })),
      check(T, 'broker', 'broker', 'export', publish({ of: 'export' })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'verified',
      'verified',
      'verified',
      'unchecked receiver_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
    ]);
  });

  it('reads a maker on the export only', async () => {
    const T = '@fixture/tasks';
    const definition = make('call', null, { name: keyed('id'), handler: keyed('run') });
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
          // A scope's receiver is named by `of`, whole; `on` never reaches it.
          check(P, 'connect', 'broker', 'instance:()>scope:topic', { ...publish, of: 'instance:()>scope:topic' }),
          check(P, 'connect', 'broker', 'instance:()>scope:topic', { ...publish, of: 'instance:()' }),
          check(P, 'connect', 'broker', 'instance:()>scope:topic', { ...publish, on: 'instance' }),
          check(P, 'connect', 'broker', 'instance:()>scope:topic', { ...publish, on: 'both' }),
        ])
      ),
      [
        'verified',
        'verified',
        'verified',
        'verified',
        'verified',
        'verified',
        'failed name_unbound',
        'verified',
        'unchecked receiver_invalid',
        'unchecked receiver_invalid',
        'unchecked receiver_invalid',
      ]
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
  // A maker's parts are slots (contract amendment 2, B2)
  // --------------------------------------------------------------------------

  it('reads a positional maker name, handler and base', async () => {
    const Q = 'fixture-queue';
    const queue = make('new', null, { name: { arg: 0 }, key_labels: { prefix: 'not_name' } });
    const checks = [
      // new Queue("emails"), its options' prefix labelled.
      check(Q, 'Queue', 'broker', 'export', queue),
      // An instance op whose name is the one the maker's positional name bound.
      check(Q, 'Queue', 'broker', 'instance:new', op('send', 'publish', { of: 'instance:new', name: { bound: 'maker' }, payload: { arg: 0 } })),
      // new Worker("emails", processor).
      check(Q, 'Worker', 'broker', 'export', make('new', null, { name: { arg: 0 }, handler: { arg: 1 } })),
      // connect(url): a positional base.
      check(Q, 'connect', 'broker', 'export', make('call', null, { base: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'verified', 'verified', 'verified']);
  });

  it("holds a maker's positional parts to the name-slot and string rules", async () => {
    const Q = 'fixture-queue';
    const checks = [
      // Strict D2 on a maker name: the options' string key is not accounted for.
      check(Q, 'Queue', 'broker', 'export', make('new', null, { name: { arg: 0 } })),
      // An unassigned positional string beside the name cannot be labelled.
      check(Q, 'Pair', 'broker', 'export', make('new', null, { name: { arg: 0 }, key_labels: {} })),
      // A base or prefix accepts a string: a port number is not one, nor is an options object.
      check(Q, 'open', 'broker', 'export', make('call', null, { base: { arg: 0 } })),
      check(Q, 'connect', 'broker', 'export', make('call', null, { prefix: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'failed name_ambiguous',
      'failed name_ambiguous',
      'failed slot_not_string',
      'failed slot_not_string',
    ]);
  });

  it('counts a rest parameter it cannot read as a string slot beside the name (strict D2, fail closed)', async () => {
    const Q = 'fixture-queue';
    const labelled = make('new', null, { name: { arg: 0 }, key_labels: { prefix: 'not_name' } });
    const checks = [
      // new BackendQueue("emails"): two overloads read the options' prefix; the
      // third takes a rest typed by a conditional on the backend, which could
      // hold that prefix too.
      check(Q, 'BackendQueue', 'broker', 'export', make('new', null, { name: { arg: 0 } })),
      // Only an unreadable rest beside the name: no label can account for it.
      check(Q, 'Spread', 'broker', 'export', make('new', null, { name: { arg: 0 } })),
      check(Q, 'Spread', 'broker', 'export', make('new', null, { name: { arg: 0 }, key_labels: { prefix: 'not_name' } })),
      // No name, no rule.
      check(Q, 'Spread', 'broker', 'export', make('new', null, {})),
      // An op beside an unreadable rest, unless the claim puts the payload in it.
      check(Q, 'emitTo', 'broker', 'export', op('receive', null, { on: 'export', name: { arg: 0 } })),
      check(Q, 'emitTo', 'broker', 'export', op('send', null, { on: 'export', name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'failed name_ambiguous',
      'failed name_ambiguous',
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'verified',
    ]);

    // With the prefix labelled, the two readable overloads hold, and the
    // instance is read through them.
    const bound = [
      check(Q, 'BackendQueue', 'broker', 'export', labelled),
      check(Q, 'BackendQueue', 'broker', 'instance:new', op('send', 'publish', { of: 'instance:new', name: { bound: 'maker' }, payload: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(bound)), ['verified', 'verified']);
  });

  it('binds no name through a maker claim with no name slot', async () => {
    const Q = 'fixture-queue';
    const checks = [
      check(Q, 'Pair', 'broker', 'export', make('new', null)),
      check(Q, 'Pair', 'broker', 'instance:new', op('send', 'publish', { name: { bound: 'maker' }, payload: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'failed name_unbound']);
  });

  // --------------------------------------------------------------------------
  // A generic receiver is read at its type-parameter defaults (contract amendment 2, B6)
  // --------------------------------------------------------------------------

  it('reads a maker that returns a generic with a default at that default', async () => {
    const R = 'fixture-rules';
    const checks = [
      check(R, 'makeDefault', 'broker', 'export', make('call', null)),
      check(R, 'makeDefault', 'broker', 'instance:()', op('send', 'send', { name: { arg: 0 }, payload: { arg: 1 } })),
      // No default and no constraint: still nothing.
      check(R, 'makeOpen', 'broker', 'export', make('call', null)),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'verified', 'unchecked maker_unresolved']);
  });

  it('reads a scope whose own type parameter defaults to its receiver\'s at that default', async () => {
    const G = 'fixture-generic';
    const checks = [
      check(G, 'Hub', 'socket', 'export', make('new', null)),
      check(G, 'Hub', 'socket', 'instance:new', scope('room', { arg: 0 }, { of: 'instance:new' })),
      check(G, 'Hub', 'socket', 'instance:new>scope:room', op('send', 'emit', { of: 'instance:new>scope:room', name: { arg: 0 }, payload: { arg: 1 } })),
      check(G, 'Hub', 'socket', 'instance:new', scope('topic', { arg: 0 }, { of: 'instance:new' })),
      check(G, 'Hub', 'socket', 'instance:new>scope:topic', op('send', 'publish', { of: 'instance:new>scope:topic', name: { bound: 'scope' }, payload: { arg: 0 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'verified', 'verified', 'verified', 'verified']);
  });

  it("reads an overload at another overload's defaults only when they build the same instance", async () => {
    const G = 'fixture-generic';
    // Only the options overload takes the claim; a call with no argument
    // resolves to the port overload, whose instance is another type.
    const checks = [
      check(G, 'line', 'broker', 'export', make('call', null, { name: keyed('id') })),
      check(G, 'line', 'broker', 'instance:()', op('send', 'send', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), ['verified', 'unchecked member_untyped']);
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

  it('reads node:<module> with the runtime types package as its home, and only that specifier (contract amendment 1)', async () => {
    const E = 'node:events';
    const runtime = [
      check(E, 'default', 'in_process_bus', 'export', make('new', null)),
      check(E, 'default', 'in_process_bus', 'instance:new', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } })),
      check(E, 'default', 'in_process_bus', 'instance:new', op('receive', 'on', { name: { arg: 0 } })),
      // The runtime's listener type, (...args: any[]) => void, says nothing about a handler.
      check(E, 'default', 'in_process_bus', 'instance:new', op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 } })),
    ];
    const response = await send(runtime);
    assert.deepStrictEqual(verdicts(response), ['verified', 'verified', 'verified', 'unchecked handler_untyped']);
    assert.strictEqual(response.modules![0].installed_version, '22.0.0');
    assert.match(String(response.modules![0].resolved_file), /node_modules\/@types\/node\/index\.d\.ts$/);
    // A class another package declares that only inherits them stays refused.
    const B = 'fixture-bus';
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(B, 'Bus', 'in_process_bus', 'export', make('new', null)),
          check(B, 'Bus', 'in_process_bus', 'instance:new', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } })),
          check(B, 'Bus', 'in_process_bus', 'instance:new', op('receive', 'on', { name: { arg: 0 } })),
        ])
      ),
      ['verified', 'failed member_inherited', 'failed member_inherited']
    );
    // The bare name is a registry package's, never the runtime module; a
    // node: module the service declares for itself is not the runtime's.
    assert.deepStrictEqual(
      verdicts(
        await send([
          check('events', 'default', 'in_process_bus', 'export', make('new', null)),
          check('node:shim', 'publish', 'in_process_bus', 'export', op('send', null, { name: { arg: 0 }, payload: { arg: 1 } })),
        ])
      ),
      ['unchecked module_local', 'unchecked module_local']
    );
    // HTTP answers as #1564 does: a node: specifier is not read as the runtime module there.
    assert.deepStrictEqual(
      verdicts(await send([check(E, 'default', 'http_client', 'export', make('call', 'create', { base: keyed('url') }))])),
      ['unchecked module_local']
    );
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

  it('refuses a name slot typed as a key of a concrete index-signature map, and reads a key of a type-parameter map as a string slot (amendment 2, B6)', async () => {
    const R = 'fixture-rules';
    const checks = [
      check(R, 'channel', 'broker', 'export', op('send', 'send', { name: { arg: 0 }, payload: { arg: 1 } })),
      // A key of the receiver's event map, a type parameter defaulting to an index signature.
      check(R, 'channel', 'broker', 'export', op('send', 'emitKey', { name: { arg: 0 }, payload: { arg: 1 } })),
      // A key of a concrete index-signature map.
      check(R, 'channel', 'broker', 'export', op('send', 'sendKey', { name: { arg: 0 }, payload: { arg: 1 } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'verified',
      'failed name_index_key',
    ]);
    // A socket client whose emitter is bound to its own reserved events and
    // whose event maps are type parameters defaulting to an index signature.
    const S = 'fixture-typed-socket';
    assert.deepStrictEqual(
      verdicts(
        await send([
          check(S, 'io', 'socket', 'export', make('call', null)),
          check(S, 'io', 'socket', 'instance:()', op('receive', 'on', { name: { arg: 0 }, handler: { arg: 1 } })),
          check(S, 'io', 'socket', 'instance:()', op('receive', 'on', { name: { arg: 0 } })),
          check(S, 'io', 'socket', 'instance:()', op('send', 'emit', { name: { arg: 0 }, payload: { arg: 1 } })),
        ])
      ),
      ['verified', 'verified', 'verified', 'verified']
    );
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
      check(R, 'channel', 'broker', 'export', make('call', member, { name: keyed('id'), handler: keyed('run') }));
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
      at(make('call', 'defineDescribed', { name: keyed('id'), handler: keyed('run') })),
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
        name: keyed('id'),
        handler: keyed('run'),
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

  it('counts a sibling a conditional types as a string when a branch takes one (strict D2, fail closed)', async () => {
    const R = 'fixture-rules';
    const at = (member: string, parts: Record<string, unknown>) =>
      check(R, 'dispatcher', 'broker', 'export', op('send', member, { on: 'export', ...parts }));
    const checks = [
      // trigger(id, payload, options): the id is the name.
      at('triggerWith', { name: { arg: 0 }, payload: { arg: 1 }, key_labels: { concurrencyKey: 'not_name' } }),
      // An options key read as the name leaves the id beside it, a string
      // through its conditional: which is the name is behaviour.
      at('triggerWith', { name: { arg: 2, key: 'concurrencyKey' }, payload: { arg: 1 } }),
      // A branch that says nothing counts no more than \`any\` does.
      at('triggerLooseWith', { name: { arg: 2, key: 'concurrencyKey' }, payload: { arg: 1 } }),
      // A positional name beside the conditional id.
      check(R, 'dispatcher', 'broker', 'export', op('receive', 'watchById', { on: 'export', name: { arg: 1 } })),
      // A keyed name beside a conditional id key, unlabelled and labelled.
      check(R, 'dispatcher', 'broker', 'export', op('receive', 'defineTyped', { on: 'export', name: keyed('queue'), handler: keyed('run') })),
      check(R, 'dispatcher', 'broker', 'export', op('receive', 'defineTyped', { on: 'export', name: keyed('queue'), handler: keyed('run'), key_labels: { id: 'not_name' } })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'verified',
      'failed name_ambiguous',
      'verified',
      'failed name_ambiguous',
      'failed name_ambiguous',
      'verified',
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
      check(K, 'createClient', 'http_client', 'export', make('call', 'create', { base: keyed('url'), ...parts }));
    const checks = [
      factory({ name: keyed('id') }),
      factory({ handler: keyed('run') }),
      factory({ prefix: keyed('prefix') }),
      // #1564's factory reads its base key on the options at argument 0, and nowhere else.
      factory({ base: { arg: 1, key: 'url' } }),
      factory({ base: { arg: 0 } }),
      check(K, 'createClient', 'http_client', 'export', op('request', 'get', { method: 'GET', name: { arg: 0 }, path: ['client'] })),
    ];
    assert.deepStrictEqual(verdicts(await send(checks)), [
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
      'unchecked claim_invalid',
    ]);
    // A factory claim steers which overload an instance is read through only
    // as #1564 states it: a base key at argument 0.
    const F = 'fixture-http-factory';
    const get = check(F, 'default', 'http_client', 'instance:create', op('request', 'get', { method: 'GET', name: { arg: 0 } }));
    assert.deepStrictEqual(
      verdicts(await send([check(F, 'default', 'http_client', 'export', make('call', 'create', { base: { arg: 1, key: 'prefixUrl' } })), get])),
      ['unchecked claim_invalid', 'verified']
    );
    assert.deepStrictEqual(
      verdicts(await send([check(F, 'default', 'http_client', 'export', make('call', 'create', { base: keyed('prefixUrl') })), get])),
      ['verified', 'unchecked factory_unresolved']
    );
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
      // There is no define op: a definition is a maker with a name slot and a handler slot.
      op('define', 'task', { name: { arg: 0 } }),
      // A maker's parts are slots (amendment 2, B2): the named keys are gone, and no part is bound.
      make('call', null, { name_key: 'id', handler_key: 'run' }),
      make('call', null, { base_key: 'url' }),
      make('call', null, { name: { bound: 'maker' } }),
      { kind: 'reserved', member: 'on', name: 'connect', at: { arg: 0 } },
      make('call', null, { name: keyed('id'), key_labels: { id: 'maybe' } }),
    ];
    for (const claim of shapes) {
      const response = await send([check(T, 'task', 'broker', 'export', claim)]);
      assert.strictEqual(response.status, 'error', JSON.stringify(claim));
    }
  });
});
