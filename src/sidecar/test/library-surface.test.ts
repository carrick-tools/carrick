/**
 * carrick#1660: `list_library_surface` lists a package's declared surface
 * with the verifier's own predicates, so a claim chosen from the listing names
 * a slot `verify_library_claims` reads the same way. The shared library store
 * runs it on packages as published, and keys every answer on the full-surface
 * hash the listing carries.
 *
 * Every package below is invented and its declarations are hand-written.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as crypto from 'node:crypto';
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
export interface Task<TPayload> {
  id: string;
  trigger(payload: TPayload): Promise<{ id: string }>;
}
export declare function task<TPayload = unknown>(options: TaskOptions<TPayload>): Task<TPayload>;
export declare const tasks: {
  trigger(id: string, payload: unknown): Promise<{ id: string }>;
};
`;

const SOCKET = `export type Ack = (response: unknown) => void;
export type Listener = (payload: unknown, ack?: Ack) => void;
export interface ClientSocket {
  emit(event: string, payload?: unknown, ack?: Ack): boolean;
  on(event: 'connect' | 'disconnect', listener: () => void): this;
  on(event: string, listener: Listener): this;
}
export declare function io(url: string, options?: { path?: string }): ClientSocket;
export declare class Server {
  constructor(options?: { port?: number });
  on(event: 'connection', listener: (socket: ClientSocket) => void): this;
  emit(event: string, payload?: unknown): boolean;
}
`;

// A client class reached one level below the export: \`new broker.Producer()\`.
// The checker prints a module's namespace type by the path of its file.
const NESTED = `export declare class Producer {
  constructor(options: { clientId: string });
  send(topic: string, payload: unknown): Promise<void>;
  inspect(): typeof import('./internal');
}
export declare const broker: {
  Producer: typeof Producer;
  version: string;
};
`;
const NESTED_INTERNAL = 'export interface Hidden { id: number }\n';

// The runtime's types package, as it declares its event module twice: the
// emitter is generic in its event map, defaulting to "no map", and its name
// slots are typed through that map (carrick#1696).
const RUNTIME = `declare module 'events' {
  type DefaultEventMap = [never];
  type EventMap<T> = Record<keyof T, any[]> | DefaultEventMap;
  type AnyRest = [...args: any[]];
  type Args<K, T> = T extends DefaultEventMap ? AnyRest : K extends keyof T ? T[K] : never;
  type Key<K, T> = T extends DefaultEventMap ? string | symbol : K | keyof T;
  type Listener<K, T, F> = T extends DefaultEventMap ? F : K extends keyof T ? (T[K] extends unknown[] ? (...args: T[K]) => void : never) : never;
  type Listener1<K, T> = Listener<K, T, (...args: any[]) => void>;
  class EventEmitter<T extends EventMap<T> = DefaultEventMap> {
    static from(source: unknown): EventEmitter;
    emit<K>(eventName: Key<K, T>, ...args: Args<K, T>): boolean;
    on<K>(eventName: Key<K, T>, listener: Listener1<K, T>): this;
  }
  import internal = require('node:events');
  namespace EventEmitter {
    export { internal as EventEmitter };
  }
  export = EventEmitter;
}
declare module 'node:events' {
  import events = require('events');
  export = events;
}
`;

// Makers generic in a message map that defaults to "no map": a name slot is
// a string only at that default. One of each maker form the listing names.
const GENERIC = `type NoMap = [never];
type Name<K, M> = M extends NoMap ? string : K & keyof M;
export interface Emitter<M> {
  emit<K>(name: Name<K, M>, payload: unknown): void;
}
export declare function createEmitter<M = NoMap>(): Emitter<M>;
export declare class Channel<M = NoMap> {
  constructor(options?: { durable?: boolean });
  send<K>(name: Name<K, M>, payload: unknown): void;
}
export declare const hub: {
  Channel: typeof Channel;
  make<M = NoMap>(): Emitter<M>;
};
`;

// A module that exports a function whole and names it \`default\` too, as
// packages written for both module systems declare.
const BOTH = `interface Client {
  send(topic: string, payload: unknown): void;
}
declare function connect(url: string): Client;
declare namespace connect {
  export { connect as default };
}
export = connect;
`;

// Members and an options key keyed by a unique symbol, beside string-keyed ones.
const SYMBOLS = `export declare const tag: unique symbol;
export interface StreamOptions {
  [tag]?: string;
  prefix?: string;
}
export declare class Stream {
  constructor(name: string, options?: StreamOptions);
  static [tag](): Stream;
  static iterate(): { [Symbol.iterator](): Iterator<unknown> };
  [Symbol.asyncIterator](): AsyncIterator<unknown>;
  [tag](name: string): void;
  read(name: string): void;
}
`;

// An unrelated package with symbol-keyed members of its own.
const OTHER = `export declare const mark: unique symbol;
export declare class Other {
  [Symbol.iterator](): Iterator<unknown>;
  [Symbol.asyncIterator](): AsyncIterator<unknown>;
  [mark](): void;
  write(name: string): void;
  close(): void;
}
`;

// A library class that extends the runtime's emitter and adds one own member.
const BUS = `/// <reference types="node" />
import EventEmitter = require('events');
export declare class Bus extends EventEmitter {
  publishLocal(topic: string, payload: unknown): void;
}
`;

function writeTree(root: string, files: Record<string, string>): void {
  for (const [rel, text] of Object.entries(files)) {
    const file = path.join(root, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  }
}

function packageJson(name: string, version: string): string {
  return JSON.stringify({ name, version, types: 'index.d.ts' });
}

/** The installed packages every service below has. */
function installed(socket = SOCKET): Record<string, string> {
  return {
    'node_modules/@fixture/tasks/package.json': packageJson('@fixture/tasks', '4.0.0'),
    'node_modules/@fixture/tasks/index.d.ts': TASKS,
    'node_modules/fixture-socket/package.json': packageJson('fixture-socket', '2.0.0'),
    'node_modules/fixture-socket/index.d.ts': socket,
    'node_modules/fixture-nested/package.json': packageJson('fixture-nested', '1.0.0'),
    'node_modules/fixture-nested/index.d.ts': NESTED,
    'node_modules/fixture-nested/internal.d.ts': NESTED_INTERNAL,
    'node_modules/@types/node/package.json': packageJson('@types/node', '22.0.0'),
    'node_modules/@types/node/index.d.ts': RUNTIME,
    'node_modules/fixture-bus/package.json': packageJson('fixture-bus', '1.0.0'),
    'node_modules/fixture-bus/index.d.ts': BUS,
    'node_modules/fixture-generic/package.json': packageJson('fixture-generic', '1.0.0'),
    'node_modules/fixture-generic/index.d.ts': GENERIC,
    'node_modules/fixture-both/package.json': packageJson('fixture-both', '1.0.0'),
    'node_modules/fixture-both/index.d.ts': BOTH,
    'node_modules/fixture-symbols/package.json': packageJson('fixture-symbols', '1.0.0'),
    'node_modules/fixture-symbols/index.d.ts': SYMBOLS,
    'node_modules/fixture-other/package.json': packageJson('fixture-other', '1.0.0'),
    'node_modules/fixture-other/index.d.ts': OTHER,
  };
}

function service(files: Record<string, string>): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-surface-'));
  writeTree(root, files);
  return root;
}

interface SurfaceKey {
  name: string;
  optional: boolean;
  accepts_string: boolean;
  function: boolean;
}
interface SurfaceParam {
  name: string;
  type: string;
  accepts_string: boolean;
  function: boolean;
  literals?: string[];
  keys?: SurfaceKey[];
}
interface SurfaceSignature {
  params: SurfaceParam[];
  returns: string;
}
interface Surface {
  package: string;
  installed_version?: string;
  reason?: string;
  truncated: number;
  exports: Array<{
    export: string;
    receivers: Array<{
      receiver: string;
      call?: SurfaceSignature[];
      construct?: SurfaceSignature[];
      members: Array<{ name: string; own: boolean; signatures: SurfaceSignature[] }>;
    }>;
  }>;
}
interface SurfaceResponse {
  status: string;
  surfaces: Surface[];
  surface_sha256?: string;
  errors?: string[];
}
interface VerdictResponse {
  status: string;
  verdicts: Array<{ verdict: string; reason?: string }>;
}

describe('list_library_surface (carrick#1660)', () => {
  const clients: SidecarClient[] = [];
  const roots: string[] = [];
  let requestId = 0;

  /** A sidecar init'd on a fresh service holding `files`. */
  async function sidecarOn(files: Record<string, string>): Promise<{ client: SidecarClient; root: string }> {
    const root = service(files);
    roots.push(root);
    const client = new SidecarClient();
    clients.push(client);
    await client.start();
    const init = await client.send<{ status: string }>({ request_id: `init-${requestId++}`, action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready');
    return { client, root };
  }

  function lister(client: SidecarClient, root: string) {
    return async (packages: string[], extra: Record<string, unknown> = {}): Promise<SurfaceResponse> => {
      const response = await client.send<SurfaceResponse>(
        { request_id: `surface-${requestId++}`, action: 'list_library_surface', from_dir: root, packages, ...extra },
        60_000
      );
      assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
      return response;
    };
  }

  let client: SidecarClient;
  let root: string;
  let surface: ReturnType<typeof lister>;

  before(async () => {
    ({ client, root } = await sidecarOn({
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'worker' }),
      'src/index.ts': 'export const service = 1;\n',
      // The service compiles against the runtime's types, as a real Node service does.
      'src/runtime.ts': '/// <reference types="node" />\nexport {};\n',
      'types/shims.d.ts': "declare module 'node:shim' {\n  export function publish(topic: string, payload: unknown): void;\n}\n",
      ...installed(),
    }));
    surface = lister(client, root);
  });

  after(async () => {
    for (const each of clients) await each.stop();
    for (const dir of roots) fs.rmSync(dir, { recursive: true, force: true });
  });

  it('lists each export, its receivers and members with the verifier predicates', async () => {
    const response = await surface(['fixture-socket', 'fixture-bus', 'fixture-not-installed']);
    const [socket, bus, missing] = response.surfaces;
    assert.strictEqual(socket.installed_version, '2.0.0');
    assert.strictEqual(socket.truncated, 0);
    assert.deepStrictEqual(socket.exports.map(e => e.export), ['Server', 'io']);
    const io = socket.exports.find(e => e.export === 'io')!;
    assert.deepStrictEqual(io.receivers.map(r => r.receiver), ['export', 'instance:()']);
    const clientSocket = io.receivers[1];
    assert.deepStrictEqual(clientSocket.members.map(m => m.name), ['emit', 'on']);
    const on = clientSocket.members.find(m => m.name === 'on')!;
    assert.ok(on.own);
    // The literal overload spells the library's own names; the general one takes a string.
    const eventParams = on.signatures.map(sig => sig.params[0]);
    assert.deepStrictEqual(eventParams[0].literals, ['connect', 'disconnect']);
    assert.strictEqual(eventParams[1].accepts_string, true);
    assert.strictEqual(on.signatures[1].params[1].function, true);
    // A class extending the runtime emitter lists the inherited members as not
    // its own, and a maker it only inherits from the runtime not at all.
    const busClass = bus.exports.find(e => e.export === 'Bus')!;
    assert.deepStrictEqual(busClass.receivers.map(r => r.receiver), ['export', 'instance:new']);
    const instance = busClass.receivers.find(r => r.receiver === 'instance:new')!;
    const own = Object.fromEntries(instance.members.map(m => [m.name, m.own]));
    assert.deepStrictEqual(own, { publishLocal: true, emit: false, on: false });
    assert.strictEqual(missing.reason, 'module_unresolved');
    assert.deepStrictEqual(missing.exports, []);
  });

  it('lists the option keys of a maker with their optional flag, and only the exports asked for', async () => {
    const response = await surface(['@fixture/tasks'], { exports: { '@fixture/tasks': ['task'] } });
    const tasks = response.surfaces[0];
    assert.deepStrictEqual(tasks.exports.map(e => e.export), ['task']);
    const maker = tasks.exports[0].receivers.find(r => r.receiver === 'export')!;
    const keys = maker.call![0].params[0].keys!;
    assert.deepStrictEqual(
      keys.map(k => [k.name, k.optional, k.accepts_string, k.function]),
      [
        ['id', false, true, false],
        ['run', false, false, true],
        ['retries', true, false, false],
      ]
    );
  });

  it('cuts signatures before names when the cap is reached, and counts what it cut', async () => {
    const full = (await surface(['fixture-socket'])).surfaces[0];
    const capped = (await surface(['fixture-socket'], { max_entries: 12 })).surfaces[0];
    assert.ok(capped.truncated > 0, 'nothing was cut');
    // Every export and every member name is still there.
    assert.deepStrictEqual(capped.exports.map(e => e.export), full.exports.map(e => e.export));
    const names = (s: Surface) =>
      s.exports.flatMap(e => e.receivers.flatMap(r => r.members.map(m => `${e.export}.${r.receiver}.${m.name}`)));
    assert.deepStrictEqual(names(capped), names(full));
  });

  it('lists a class one level below an export as instance:new:<member>, a receiver the verifier reads', async () => {
    const nested = (await surface(['fixture-nested'])).surfaces[0];
    const broker = nested.exports.find(e => e.export === 'broker')!;
    assert.deepStrictEqual(broker.receivers.map(r => r.receiver), ['export', 'instance:new:Producer']);
    const producer = broker.receivers[1];
    assert.deepStrictEqual(producer.members.map(m => m.name), ['send', 'inspect']);

    // The receiver the listing names is one the verifier reads claims on.
    const check = (claim_id: string, receiver: string, claim: Record<string, unknown>) => ({
      claim_id,
      package: 'fixture-nested',
      export: 'broker',
      role: 'broker',
      receiver,
      claim,
    });
    const verdicts = await client.send<VerdictResponse>(
      {
        request_id: `claims-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: root,
        checks: [
          check('maker', 'export', { kind: 'make', form: 'new', member: 'Producer' }),
          check('send', 'instance:new:Producer', {
            kind: 'op',
            op: 'send',
            member: 'send',
            on: 'instance',
            name: { arg: 0 },
            payload: { arg: 1 },
          }),
        ],
      },
      60_000
    );
    assert.deepStrictEqual(
      verdicts.verdicts.map(v => v.verdict),
      ['verified', 'verified']
    );
  });

  it('lists node:<module> from the installed runtime types package, and not a block only the service writes', async () => {
    const response = await surface(['node:events', 'node:shim']);
    const [events, shim] = response.surfaces;
    assert.strictEqual(events.reason, undefined, JSON.stringify(events));
    assert.strictEqual(events.installed_version, '22.0.0');
    // A module that exports a class whole exports its statics (\`from\`), but
    // not its \`prototype\`: no service imports that. A default import gets
    // the class itself (\`default\`).
    assert.deepStrictEqual(events.exports.map(e => e.export), ['EventEmitter', 'default', 'from']);
    const emitter = events.exports[0];
    // The runtime's own static maker is the runtime module's.
    assert.deepStrictEqual(emitter.receivers.map(r => r.receiver), [
      'export',
      'instance:new',
      'instance:from',
      'instance:new:EventEmitter',
    ]);
    const instance = emitter.receivers.find(r => r.receiver === 'instance:new')!;
    // The runtime module's home is the runtime's types package: its members are its own.
    assert.deepStrictEqual(
      instance.members.map(m => [m.name, m.own]),
      [
        ['emit', true],
        ['on', true],
      ]
    );
    assert.strictEqual(shim.reason, 'module_local');
    assert.deepStrictEqual(shim.exports, []);
  });

  /** The first parameter of each signature of `member` on `receiver` of `exportName`. */
  function nameSlots(listed: Surface, exportName: string, receiver: string, member: string) {
    const exported = listed.exports.find(e => e.export === exportName);
    assert.ok(exported, `${listed.package} lists no ${exportName}`);
    const made = exported.receivers.find(r => r.receiver === receiver);
    assert.ok(made, `${exportName} lists no ${receiver}`);
    const callable = made.members.find(m => m.name === member);
    assert.ok(callable, `${exportName} ${receiver} lists no ${member}`);
    return callable.signatures.map(sig => ({ type: sig.params[0].type, accepts_string: sig.params[0].accepts_string }));
  }

  it('lists what a generic maker builds at its declared type-parameter defaults, as the verifier reads it (carrick#1696)', async () => {
    const [generic, events] = (await surface(['fixture-generic', 'node:events'])).surfaces;
    assert.deepStrictEqual(
      generic.exports.map(e => [e.export, e.receivers.map(r => r.receiver)]),
      [
        ['Channel', ['export', 'instance:new']],
        ['createEmitter', ['export', 'instance:()']],
        ['hub', ['export', 'instance:new:Channel', 'instance:make']],
      ]
    );
    // Each maker form, built with no type argument, makes the map "no map",
    // where a name is a string. With the map left open the slot is
    // `Name<K, M>`, which takes no string.
    const atDefault = [{ type: 'string', accepts_string: true }];
    assert.deepStrictEqual(nameSlots(generic, 'createEmitter', 'instance:()', 'emit'), atDefault);
    assert.deepStrictEqual(nameSlots(generic, 'Channel', 'instance:new', 'send'), atDefault);
    assert.deepStrictEqual(nameSlots(generic, 'hub', 'instance:new:Channel', 'send'), atDefault);
    assert.deepStrictEqual(nameSlots(generic, 'hub', 'instance:make', 'emit'), atDefault);
    // The runtime's emitter: \`Key<K, T>\` at its default map is \`string | symbol\`.
    const runtime = [{ type: 'string | symbol', accepts_string: true }];
    for (const receiver of ['instance:new', 'instance:new:EventEmitter']) {
      assert.deepStrictEqual(nameSlots(events, 'EventEmitter', receiver, 'emit'), runtime, receiver);
      assert.deepStrictEqual(nameSlots(events, 'EventEmitter', receiver, 'on'), runtime, receiver);
    }

    // The claim the listing allows is the one the verifier verifies.
    const check = (claim_id: string, pkg: string, exportName: string, receiver: string, claim: Record<string, unknown>) => ({
      claim_id,
      package: pkg,
      export: exportName,
      role: 'in_process_bus',
      receiver,
      claim,
    });
    const send = (member: string, of: string) => ({ kind: 'op', op: 'send', member, of, name: { arg: 0 }, payload: { arg: 1 } });
    const verdicts = await client.send<VerdictResponse>(
      {
        request_id: `claims-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: root,
        checks: [
          check('emitter', 'fixture-generic', 'createEmitter', 'export', { kind: 'make', form: 'call', member: null }),
          check('emit', 'fixture-generic', 'createEmitter', 'instance:()', send('emit', 'instance:()')),
          check('channel', 'fixture-generic', 'Channel', 'export', { kind: 'make', form: 'new', member: null }),
          check('send', 'fixture-generic', 'Channel', 'instance:new', send('send', 'instance:new')),
          check('hub-channel', 'fixture-generic', 'hub', 'export', { kind: 'make', form: 'new', member: 'Channel' }),
          check('hub-send', 'fixture-generic', 'hub', 'instance:new:Channel', send('send', 'instance:new:Channel')),
          check('hub-make', 'fixture-generic', 'hub', 'export', { kind: 'make', form: 'call', member: 'make' }),
          check('hub-emit', 'fixture-generic', 'hub', 'instance:make', send('emit', 'instance:make')),
          check('runtime', 'node:events', 'EventEmitter', 'export', { kind: 'make', form: 'new', member: null }),
          check('runtime-emit', 'node:events', 'EventEmitter', 'instance:new', send('emit', 'instance:new')),
        ],
      },
      60_000
    );
    assert.deepStrictEqual(
      verdicts.verdicts.map(v => `${v.verdict}${v.reason ? ` ${v.reason}` : ''}`),
      Array(10).fill('verified')
    );
  });

  it('lists default for a module that exports a value whole, as a default import gets it (carrick#1696)', async () => {
    const events = (await surface(['node:events'], { exports: { 'node:events': ['default'] } })).surfaces[0];
    assert.deepStrictEqual(events.exports.map(e => e.export), ['default']);
    // \`import EventEmitter from "node:events"\` is the class: the receivers
    // and slots the class's own name lists.
    assert.deepStrictEqual(events.exports[0].receivers.map(r => r.receiver), [
      'export',
      'instance:new',
      'instance:from',
      'instance:new:EventEmitter',
    ]);
    assert.deepStrictEqual(nameSlots(events, 'default', 'instance:new', 'emit'), [{ type: 'string | symbol', accepts_string: true }]);
    const verdicts = await client.send<VerdictResponse>(
      {
        request_id: `claims-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: root,
        checks: [
          { claim_id: 'make', package: 'node:events', export: 'default', role: 'in_process_bus', receiver: 'export', claim: { kind: 'make', form: 'new', member: null } },
          {
            claim_id: 'emit',
            package: 'node:events',
            export: 'default',
            role: 'in_process_bus',
            receiver: 'instance:new',
            claim: { kind: 'op', op: 'send', member: 'emit', of: 'instance:new', name: { arg: 0 }, payload: { arg: 1 } },
          },
        ],
      },
      60_000
    );
    assert.deepStrictEqual(verdicts.verdicts.map(v => v.verdict), ['verified', 'verified']);

    // A module with named exports and no \`export =\` lists no \`default\`, though
    // a default import of a declaration file without \`__esModule\` gets the
    // whole module under esModuleInterop.
    const socket = (await surface(['fixture-socket'])).surfaces[0];
    assert.ok(!socket.exports.some(e => e.export === 'default'), JSON.stringify(socket.exports.map(e => e.export)));
    // A module that names its own \`default\` lists that one, once.
    const both = (await surface(['fixture-both'])).surfaces[0];
    assert.deepStrictEqual(both.exports.map(e => e.export), ['default']);
    // Only the exports asked for.
    const statics = (await surface(['node:events'], { exports: { 'node:events': ['from'] } })).surfaces[0];
    assert.deepStrictEqual(statics.exports.map(e => e.export), ['from']);
  });

  it('lists no default where a default import does not compile, as the verifier answers there', async () => {
    // Without esModuleInterop, \`import x from "node:events"\` is an error.
    const noInterop = JSON.stringify({
      compilerOptions: { target: 'es2020', module: 'commonjs', moduleResolution: 'node', strict: true, skipLibCheck: true },
      include: ['src/**/*.ts'],
    });
    const bare = await sidecarOn({
      'tsconfig.json': noInterop,
      'src/runtime.ts': '/// <reference types="node" />\nexport {};\n',
      ...installed(),
    });
    const events = (await lister(bare.client, bare.root)(['node:events'])).surfaces[0];
    assert.strictEqual(events.reason, undefined, JSON.stringify(events));
    assert.deepStrictEqual(events.exports.map(e => e.export), ['EventEmitter', 'from']);
    const verdicts = await bare.client.send<VerdictResponse>(
      {
        request_id: `claims-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: bare.root,
        checks: [
          { claim_id: 'make', package: 'node:events', export: 'default', role: 'in_process_bus', receiver: 'export', claim: { kind: 'make', form: 'new', member: null } },
        ],
      },
      60_000
    );
    assert.deepStrictEqual(verdicts.verdicts.map(v => [v.verdict, v.reason]), [['unchecked', 'export_missing']]);
  });

  it('lists no member or key keyed by a symbol, and such a key never blocks a name', async () => {
    const symbols = (await surface(['fixture-symbols'])).surfaces[0];
    const stream = symbols.exports.find(e => e.export === 'Stream')!;
    // A static keyed by a symbol is no maker a receiver id can name, and what
    // \`iterate\` builds has no member a claim can name.
    assert.deepStrictEqual(stream.receivers.map(r => r.receiver), ['export', 'instance:new']);
    const instance = stream.receivers.find(r => r.receiver === 'instance:new')!;
    // \`[Symbol.asyncIterator]\` and \`[tag]\` have no name a claim can carry.
    assert.deepStrictEqual(instance.members.map(m => m.name), ['read']);
    const constructor = stream.receivers.find(r => r.receiver === 'export')!.construct![0];
    assert.deepStrictEqual(constructor.params[1].keys!.map(k => k.name), ['prefix']);
    assert.ok(!JSON.stringify(symbols.exports).includes('__@'), JSON.stringify(symbols.exports));

    // The string key \`[tag]\` beside the name is not one D2 counts: labelling
    // \`prefix\` accounts for every other string key.
    const verdicts = await client.send<VerdictResponse>(
      {
        request_id: `claims-${requestId++}`,
        action: 'verify_library_claims',
        from_dir: root,
        checks: [
          {
            claim_id: 'stream',
            package: 'fixture-symbols',
            export: 'Stream',
            role: 'broker',
            receiver: 'export',
            claim: { kind: 'make', form: 'new', member: null, name: { arg: 0 }, key_labels: { prefix: 'not_name' } },
          },
          {
            claim_id: 'stream-unlabelled',
            package: 'fixture-symbols',
            export: 'Stream',
            role: 'broker',
            receiver: 'export',
            claim: { kind: 'make', form: 'new', member: null, name: { arg: 0 } },
          },
        ],
      },
      60_000
    );
    assert.deepStrictEqual(
      verdicts.verdicts.map(v => `${v.verdict}${v.reason ? ` ${v.reason}` : ''}`),
      ['verified', 'failed name_ambiguous']
    );
  });

  it('never reads a member keyed by a symbol, whatever name a claim gives it', async () => {
    // The checker names \`static [tag]()\` \`__@tag@<symbol id>\`. A fresh process
    // gives the symbol a small id, so a claim for every id up to 2000 names it
    // if any claim can.
    const fresh = await sidecarOn({ 'tsconfig.json': TSCONFIG, ...installed() });
    const checks = Array.from({ length: 2000 }, (_, i) => ({
      claim_id: `id-${i + 1}`,
      package: 'fixture-symbols',
      export: 'Stream',
      role: 'broker',
      receiver: 'export',
      claim: { kind: 'make', form: 'call', member: `__@tag@${i + 1}` },
    }));
    const verdicts = await fresh.client.send<VerdictResponse>(
      { request_id: `claims-${requestId++}`, action: 'verify_library_claims', from_dir: fresh.root, checks, budget_ms: 60_000 },
      60_000
    );
    const answers = new Set(verdicts.verdicts.map(v => `${v.verdict}${v.reason ? ` ${v.reason}` : ''}`));
    assert.deepStrictEqual([...answers], ['failed member_missing']);
  });

  it('hashes a package the same whatever the program read before it', async () => {
    const alone = await sidecarOn({ 'tsconfig.json': TSCONFIG, ...installed() });
    const first = await lister(alone.client, alone.root)(['fixture-symbols']);

    // Another process that listed an unrelated package first.
    const after = await sidecarOn({ 'tsconfig.json': TSCONFIG, ...installed() });
    await lister(after.client, after.root)(['fixture-other', 'node:events']);
    const second = await lister(after.client, after.root)(['fixture-symbols']);
    assert.strictEqual(second.surface_sha256, first.surface_sha256);

    // One program that imports the unrelated package earlier.
    const together = await lister(after.client, after.root)(['fixture-other', 'fixture-symbols']);
    assert.deepStrictEqual(together.surfaces[1].exports, first.surfaces[0].exports);
  });

  it('carries one full-surface hash, the same from any directory and with no directory in it', async () => {
    const first = await surface(['fixture-nested', 'fixture-socket']);
    const text = JSON.stringify(first.surfaces.map(s => s.exports));
    // The checker prints a module's namespace type by the path of its file.
    assert.ok(text.includes('<root>/node_modules/fixture-nested/internal'), text);
    for (const dir of [root, fs.realpathSync(root)]) assert.ok(!text.includes(dir), `listing names ${dir}`);

    // The hash is the sha256 of the listed specifiers, in order, with their exports.
    const listed = [...first.surfaces].sort((a, b) => (a.package < b.package ? -1 : a.package > b.package ? 1 : 0));
    const expected = crypto
      .createHash('sha256')
      .update(JSON.stringify(listed.map(s => [s.package, s.exports])))
      .digest('hex');
    assert.strictEqual(first.surface_sha256, expected);

    // Another service with the same packages hashes the same.
    const other = await sidecarOn({ 'tsconfig.json': TSCONFIG, ...installed() });
    const second = await lister(other.client, other.root)(['fixture-socket', 'fixture-nested']);
    assert.strictEqual(second.surface_sha256, first.surface_sha256);

    // One declaration changed is another hash.
    const changed = await sidecarOn({
      'tsconfig.json': TSCONFIG,
      ...installed(SOCKET.replace('emit(event: string, payload?: unknown): boolean;\n}', 'emit(event: string): boolean;\n}')),
    });
    const third = await lister(changed.client, changed.root)(['fixture-socket', 'fixture-nested']);
    assert.notStrictEqual(third.surface_sha256, first.surface_sha256);
  });

  it('leaves a specifier that lists nothing out of the hash', async () => {
    const alone = await surface(['fixture-socket']);
    const withMissing = await surface(['fixture-socket', 'fixture-not-installed', 'node:shim']);
    assert.strictEqual(withMissing.surface_sha256, alone.surface_sha256);
  });

  it('lists a package directory that has no tsconfig and no manifest of its own', async () => {
    const bare = await sidecarOn(installed());
    const listed = (await lister(bare.client, bare.root)(['fixture-socket'])).surfaces[0];
    assert.strictEqual(listed.reason, undefined, JSON.stringify(listed));
    assert.deepStrictEqual(listed.exports.map(e => e.export), ['Server', 'io']);
  });
});
