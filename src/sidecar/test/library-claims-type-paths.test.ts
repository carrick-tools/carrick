/**
 * carrick#3046 (amendment 7, part 1): a type receiver names its type by a
 * path. `type:A.B` is a type the export's namespace declares, and
 * `type:<module>#A.B` one another module (a types package) declares. The
 * verifier reads the type from its probe line's own type reference, so a
 * generic with no default is read at its declared type (ruling R2); a type's
 * key is the set of declarations it stands on, bases included; `bound:
 * maker|scope` on a type receiver holds when a verified maker returns the
 * same declarations; and `yields.package` names the package that declares
 * the handed type.
 *
 * Every package below is invented and its declarations are hand-written.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import type { LibraryCheck } from '../src/types.js';

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

// A module exported whole, its types inside the namespace.
const APP = `declare function App(): App.Application;
declare namespace App {
  interface Context<S = {}> {
    body: unknown;
    state: S;
    send(body: unknown): void;
  }
  interface Application {
    use(fn: (ctx: Context) => void): this;
  }
}
export = App;
`;

// A types package: the router interfaces, and a generic with no default.
const CORE = `export type Handler = (ctx: unknown) => void;
export interface Router {
  get(path: string, ...h: Handler[]): this;
  publish(payload: unknown): void;
}
export interface Route<P extends string = string> {
  get(...h: Handler[]): this;
}
export interface EventMap {
  [event: string]: unknown;
}
export interface Broadcast<E extends EventMap> {
  emit(ev: keyof E & string, ...a: unknown[]): boolean;
  publish(payload: E): void;
}
export declare const notAType: { publish(payload: unknown): void };
`;

// Re-exports the types package and makes its router.
const SERVER = `import { Router } from 'fixture-core';
export * from 'fixture-core';
export interface AppRouter extends Router {
  prefix: string;
  publishAll(payload: unknown): void;
}
export declare function server(name?: string): Router;
export declare function appServer(name?: string): AppRouter;
`;

// A route handler handed another package's context.
const ROUTER = `import App = require('fixture-app');
export interface Router {
  get(path: string, handler: (ctx: App.Context) => void): this;
}
export declare function router(): Router;
`;

let sequence = 0;
const check = (pkg: string, exported: string, receiver: string, claim: Record<string, unknown>): LibraryCheck =>
  ({
    claim_id: `t${sequence++}`,
    package: pkg,
    export: exported,
    role: 'server_framework',
    receiver,
    claim,
  }) as unknown as LibraryCheck;

const send = (pkg: string, exported: string, of: string, extra: Record<string, unknown> = {}) =>
  check(pkg, exported, of, { kind: 'op', op: 'send', member: 'publish', payload: { arg: 0 }, of, ...extra });
const maker = (exported: string) =>
  check('fixture-server', exported, 'export', { kind: 'make', form: 'call', member: null, name: { arg: 0 } });
const route = (yields?: Record<string, unknown>) =>
  check('fixture-router', 'router', 'instance:()', {
    kind: 'op',
    op: 'receive',
    member: 'get',
    of: 'instance:()',
    name: { arg: 0 },
    handler: { arg: 1 },
    method: 'GET',
    ...(yields ? { yields } : {}),
  });

describe('verify_library_claims: type receiver paths (carrick#3046)', () => {
  let root: string;
  let verifier: LibraryClaimsVerifier;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-type-paths-'));
    const files: Record<string, string> = {
      'tsconfig.json': TSCONFIG,
      'package.json': JSON.stringify({ name: 'svc' }),
      'src/index.ts': 'export const service = 1;\n',
    };
    for (const [name, text] of [
      ['fixture-app', APP],
      ['fixture-core', CORE],
      ['fixture-server', SERVER],
      ['fixture-router', ROUTER],
    ]) {
      files[`node_modules/${name}/package.json`] = JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' });
      files[`node_modules/${name}/index.d.ts`] = text;
    }
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

  const verdicts = (checks: LibraryCheck[]): string[] =>
    verifier.run(root, checks, 60_000, undefined, { readReplies: true }).semantics.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));

  /** The declaration names a verified outcome's `receiverTypes` carry, by claim id. */
  const receiverTypes = (checks: LibraryCheck[]): Map<string, string[]> => {
    const seen = new Map<string, string[]>();
    verifier.run(root, checks, 60_000, undefined, {
      readReplies: true,
      observe: (c, outcome) => {
        if (outcome.verdict !== 'verified') return;
        const names = (outcome.receiverTypes ?? []).map(d => (d as { name?: { getText(): string } }).name?.getText() ?? '?');
        seen.set(c.claim_id, names);
      },
    });
    return seen;
  };

  const contextSend = () =>
    check('fixture-app', 'default', 'type:Context', {
      kind: 'op',
      op: 'send',
      member: 'send',
      payload: { arg: 0 },
      of: 'type:Context',
    });

  it('1. reads a type the export`s namespace declares, and carries its declarations', () => {
    const c = contextSend();
    assert.deepStrictEqual(verdicts([c]), ['verified']);
    assert.deepStrictEqual(receiverTypes([c]).get(c.claim_id), ['Context']);
  });

  it('2. reads a type another module declares, named by `type:<module>#<Path>`', () => {
    assert.deepStrictEqual(verdicts([send('fixture-server', 'server', 'type:fixture-core#Router')]), ['verified']);
  });

  it('3. reads a generic with no default at its declared type (ruling R2)', () => {
    assert.deepStrictEqual(verdicts([send('fixture-server', 'server', 'type:fixture-core#Broadcast')]), ['verified']);
    // The bare `type` receiver reads it the same way.
    assert.deepStrictEqual(verdicts([send('fixture-core', 'Broadcast', 'type')]), ['verified']);
  });

  it('4. keys a type by its declarations, bases included', () => {
    // `appServer()` returns an interface that extends `Router`: it builds a `Router` too.
    const bound = send('fixture-server', 'server', 'type:fixture-core#Router', { name: { bound: 'maker' } });
    assert.deepStrictEqual(verdicts([maker('appServer'), bound]), ['verified', 'verified']);
    // The subclass`s own receiver carries the base`s declaration as well.
    const own = send('fixture-server', 'appServer', 'type:fixture-server#AppRouter', { member: 'publishAll' });
    assert.deepStrictEqual(verdicts([own]), ['verified']);
    assert.deepStrictEqual(receiverTypes([own]).get(own.claim_id)?.sort(), ['AppRouter', 'Router']);
  });

  it('5. binds a name by type: a verified maker of the same type, else `name_unbound`', () => {
    const bound = send('fixture-server', 'server', 'type:fixture-core#Router', { name: { bound: 'maker' } });
    assert.deepStrictEqual(verdicts([maker('server'), bound]), ['verified', 'verified']);
    assert.deepStrictEqual(verdicts([bound]), ['failed name_unbound']);
  });

  it('6. `yields.package` matches the handed type in that package`s home', () => {
    const yields = { param: 0, export: 'Context', side: 'server' };
    const made = check('fixture-router', 'router', 'export', { kind: 'make', form: 'call', member: null });
    assert.deepStrictEqual(verdicts([made, route({ ...yields, package: 'fixture-app' })]), ['verified', 'verified']);
    assert.deepStrictEqual(verdicts([made, route(yields)]), ['verified', 'failed yields_not_export']);
  });

  it('7. a name with no type meaning is `type_unresolved`', () => {
    assert.deepStrictEqual(verdicts([send('fixture-server', 'server', 'type:fixture-core#notAType')]), ['unchecked type_unresolved']);
    assert.deepStrictEqual(verdicts([send('fixture-app', 'default', 'type:Missing')]), ['unchecked type_unresolved']);
  });

  it('refuses a path on another receiver, and a scope step after one', () => {
    const elsewhere = { ...contextSend(), receiver: 'export' } as LibraryCheck;
    assert.deepStrictEqual(verdicts([elsewhere]), ['unchecked receiver_invalid']);
    assert.deepStrictEqual(verdicts([send('fixture-app', 'default', 'type:Context>scope:x')]), ['unchecked claim_invalid']);
  });
});
