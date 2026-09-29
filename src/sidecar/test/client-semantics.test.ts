/**
 * carrick#1564: `verify_client_semantics` checks each library-semantics claim
 * against the package's own type declarations.
 *
 * Every package below is invented and its declarations are hand-written, so
 * the tests pin the predicates rather than one library's current types. The
 * two answered clients mirror the contract sample: a callable instance built
 * by `create({ baseURL })` that takes a config object, and one built by
 * `create({ prefixUrl })` that takes `(path, options)`. A third package
 * declares one member per failure, and the rest cover what cannot be read.
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

const CONFIG_CLIENT = `export type Method =
  | 'get' | 'GET' | 'post' | 'POST' | 'put' | 'PUT' | 'patch' | 'PATCH' | 'delete' | 'DELETE';
export interface RequestConfig<D = any> {
  url?: string;
  method?: Method;
  baseURL?: string;
  data?: D;
  headers?: Record<string, string>;
}
export interface Instance {
  <T = any>(config: RequestConfig): Promise<T>;
  request<T = any, D = any>(config: RequestConfig<D>): Promise<T>;
  get<T = any>(url: string, config?: RequestConfig): Promise<T>;
  post<T = any, D = any>(url: string, data?: D, config?: RequestConfig<D>): Promise<T>;
}
export interface Static extends Instance {
  create(config?: RequestConfig): Instance;
}
declare const client: Static;
export default client;
export declare const named: Static;
`;

const PREFIX_CLIENT = `export interface Options {
  prefixUrl?: string | { href: string };
  method?: string;
  json?: unknown;
  headers?: Record<string, string>;
}
export interface ResponsePromise extends Promise<unknown> {
  json<T = unknown>(): Promise<T>;
}
export interface Client {
  (url: string, options?: Options): ResponsePromise;
  get(url: string, options?: Options): ResponsePromise;
  post(url: string, options?: Options): ResponsePromise;
  put(...args: [url: string, options?: Options]): ResponsePromise;
  delete(...parts: string[]): ResponsePromise;
  patch?(url: string, options?: Options): ResponsePromise;
  create(defaults?: Options): Client;
}
declare const client: Client;
export default client;
`;

// One member per way a claim can be wrong.
const WRONG_CLIENT = `export interface Instance {
  get(url: number): Promise<unknown>;
  post(url: string): Promise<unknown>;
  put(url: string, body: { a: string }): Promise<unknown>;
  patch(url: string, options: Record<string, string>): Promise<unknown>;
  delete(url: string, options: { headers?: Record<string, string> }): Promise<unknown>;
  head: string;
  send(): Promise<unknown>;
  call(config: { url?: string }): Promise<unknown>;
  fire(config: { url: number; method: string }): Promise<unknown>;
  shout(config: { url: string; method: 'CONNECT' | 'trace' }): Promise<unknown>;
  go(path: string, options: { verb?: string }): Promise<unknown>;
  run(path: string, options: { method: number }): Promise<unknown>;
  walk(path: number, options: { method: string }): Promise<unknown>;
  submit(config: { url: string; method: string }): Promise<unknown>;
  trace(url: string, options: string): Promise<unknown>;
  ping(): Promise<unknown>;
  ping(config: { url?: string }): Promise<unknown>;
  index(config: { [key: string]: string }): Promise<unknown>;
}
export interface Static extends Instance {
  make: string;
  build(): Instance;
  create(options: { timeout?: number }): Instance;
  spawn(options: { baseURL: number }): Instance;
}
declare const client: Static;
export default client;
`;

// The factory's option is declared, but what it builds is \`any\`.
const BROKEN_FACTORY = `export interface Instance {
  get(url: string): Promise<unknown>;
}
export interface Static extends Instance {
  create(options: { baseURL: string }): any;
}
declare const client: Static;
export default client;
`;

// The instance comes from the overload that takes an options object, or from
// a factory's only signature; a factory with several overloads and none of
// them taking one builds nothing readable.
const OVERLOADED_FACTORY = `export interface Named {
  label: string;
}
export interface Instance {
  get(url: string): Promise<unknown>;
}
export interface Static {
  create(name: string): Named;
  create(options: { baseURL?: string }): Instance;
  make(name: string): Instance;
  make(port: number): Instance;
  solo(): Instance;
}
declare const client: Static;
export default client;
`;

const ANY_CLIENT = `declare const client: any;
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

function packageJson(name: string, version: string, entry: Record<string, string>): string {
  return JSON.stringify({ name, version, ...entry });
}

type Claim = Record<string, unknown> & { kind: string };
interface Check {
  claim_id: string;
  package: string;
  export: string;
  receiver: string;
  claim: Claim;
}
interface Result {
  claim_id: string;
  receiver: string;
  verdict: 'verified' | 'failed' | 'unchecked';
  reason?: string;
}
interface Module {
  package: string;
  resolved_file?: string;
  installed_version?: string;
  reason?: string;
}
interface Response {
  status: string;
  semantics?: Result[];
  semantics_modules?: Module[];
  errors?: string[];
}

function check(pkg: string, receiver: string, claim: Claim, exported = 'default'): Check {
  const member = claim.member === null ? '()' : String(claim.member);
  const args = claim.kind === 'request' || claim.kind === 'request_body' ? `:${claim.args}` : '';
  return {
    claim_id: `${pkg}:${exported}:${claim.kind}:${member}${args}`,
    package: pkg,
    export: exported,
    receiver,
    claim,
  };
}

/** Every claim the contract sample derives for one client, on both receivers. */
function sampleChecks(pkg: string, factory: Claim, instanceClaims: Claim[]): Check[] {
  const checks = [check(pkg, 'export', factory)];
  for (const claim of instanceClaims) {
    checks.push(check(pkg, 'export', claim));
    checks.push(check(pkg, `instance:${factory.member}`, claim));
  }
  return checks;
}

const CONFIG_CHECKS = sampleChecks(
  '@fixture/http',
  { kind: 'factory', member: 'create', base_url_key: 'baseURL' },
  [
    { kind: 'verb', member: 'get', method: 'GET' },
    { kind: 'verb', member: 'post', method: 'POST' },
    { kind: 'verb_body', member: 'get', args: 'path_options' },
    { kind: 'verb_body', member: 'get', args: 'path_options', body_key: 'data' },
    { kind: 'verb_body', member: 'post', args: 'path_body' },
    { kind: 'request', member: 'request', args: 'config', url_key: 'url', method_key: 'method' },
    { kind: 'request', member: null, args: 'config', url_key: 'url', method_key: 'method' },
    { kind: 'request_body', member: 'request', args: 'config', body_key: 'data' },
    { kind: 'request_body', member: null, args: 'config', body_key: 'data' },
  ]
);

const PREFIX_CHECKS = sampleChecks(
  'fixture-prefix-http',
  { kind: 'factory', member: 'create', base_url_key: 'prefixUrl' },
  [
    { kind: 'verb', member: 'get', method: 'GET' },
    { kind: 'verb', member: 'post', method: 'POST' },
    { kind: 'verb_body', member: 'post', args: 'path_options', body_key: 'json' },
    // Read through a tuple rest parameter and an array rest parameter.
    { kind: 'verb', member: 'put', method: 'PUT' },
    { kind: 'verb_body', member: 'put', args: 'path_options', body_key: 'json' },
    { kind: 'verb', member: 'delete', method: 'DELETE' },
    // An optional member is still a declared callable property.
    { kind: 'verb', member: 'patch', method: 'PATCH' },
    { kind: 'request', member: null, args: 'path_options', method_key: 'method' },
    { kind: 'request_body', member: null, args: 'path_options', body_key: 'json' },
  ]
);

const W = 'fixture-wrong-http';
/** Each wrong claim beside the verdict and reason it must get. */
const WRONG_CASES: Array<[Claim, Result['verdict'], string]> = [
  [{ kind: 'factory', member: 'absent', base_url_key: 'baseURL' }, 'failed', 'member_missing'],
  [{ kind: 'factory', member: 'make', base_url_key: 'baseURL' }, 'failed', 'member_not_callable'],
  [{ kind: 'factory', member: 'build', base_url_key: 'baseURL' }, 'failed', 'param_missing'],
  [{ kind: 'factory', member: 'create', base_url_key: 'baseURL' }, 'failed', 'key_missing'],
  [{ kind: 'factory', member: 'spawn', base_url_key: 'baseURL' }, 'failed', 'key_not_string'],
  [{ kind: 'verb', member: 'get', method: 'POST' }, 'failed', 'method_not_member_verb'],
  [{ kind: 'verb', member: 'fetch', method: 'FETCH' }, 'failed', 'method_not_member_verb'],
  [{ kind: 'verb', member: 'options', method: 'OPTIONS' }, 'failed', 'member_missing'],
  [{ kind: 'verb', member: 'head', method: 'HEAD' }, 'failed', 'member_not_callable'],
  [{ kind: 'verb', member: 'get', method: 'GET' }, 'failed', 'path_not_string'],
  [{ kind: 'verb_body', member: 'post', args: 'path_body' }, 'failed', 'param_missing'],
  [{ kind: 'verb_body', member: 'put', args: 'path_body' }, 'failed', 'body_not_open'],
  [{ kind: 'verb_body', member: 'patch', args: 'path_options' }, 'failed', 'options_not_object'],
  // A string has properties, but it is not an options object.
  [{ kind: 'verb_body', member: 'trace', args: 'path_options' }, 'failed', 'options_not_object'],
  [
    { kind: 'verb_body', member: 'delete', args: 'path_options', body_key: 'body' },
    'failed',
    'key_missing',
  ],
  [
    { kind: 'request', member: 'absent', args: 'config', url_key: 'url', method_key: 'method' },
    'failed',
    'member_missing',
  ],
  [
    { kind: 'request', member: null, args: 'config', url_key: 'url', method_key: 'method' },
    'failed',
    'member_not_callable',
  ],
  [
    { kind: 'request', member: 'send', args: 'config', url_key: 'url', method_key: 'method' },
    'failed',
    'param_missing',
  ],
  [
    { kind: 'request', member: 'call', args: 'config', url_key: 'url', method_key: 'method' },
    'failed',
    'key_missing',
  ],
  // The overload that got furthest names the reason, not the first one.
  [
    { kind: 'request', member: 'ping', args: 'config', url_key: 'url', method_key: 'method' },
    'failed',
    'key_missing',
  ],
  // An index signature does not declare a key.
  [
    { kind: 'request', member: 'index', args: 'config', url_key: 'url', method_key: 'method' },
    'failed',
    'key_missing',
  ],
  [
    { kind: 'request', member: 'fire', args: 'config', url_key: 'url', method_key: 'method' },
    'failed',
    'key_not_string',
  ],
  [
    { kind: 'request', member: 'shout', args: 'config', url_key: 'url', method_key: 'method' },
    'failed',
    'key_not_string',
  ],
  [
    { kind: 'request', member: 'walk', args: 'path_options', method_key: 'method' },
    'failed',
    'param_missing',
  ],
  [
    { kind: 'request', member: 'go', args: 'path_options', method_key: 'method' },
    'failed',
    'key_missing',
  ],
  [
    { kind: 'request', member: 'run', args: 'path_options', method_key: 'method' },
    'failed',
    'key_not_string',
  ],
  [
    { kind: 'request_body', member: 'submit', args: 'config', body_key: 'data' },
    'failed',
    'key_missing',
  ],
  [
    { kind: 'request_body', member: 'send', args: 'config', body_key: 'data' },
    'failed',
    'param_missing',
  ],
];

const VERB_GET: Claim = { kind: 'verb', member: 'get', method: 'GET' };

function pairs(results: Result[] | Check[]): string[] {
  return results.map(r => `${r.claim_id} @ ${r.receiver}`);
}

describe('verify_client_semantics (carrick#1564)', () => {
  let root: string;
  let bare: string;
  let client: SidecarClient;
  let requestId = 0;

  const verify = (fromDir: string, checks: Check[], budgetMs?: number) =>
    client.send<Response>(
      {
        request_id: `semantics-${requestId++}`,
        action: 'verify_client_semantics',
        from_dir: fromDir,
        checks,
        ...(budgetMs === undefined ? {} : { budget_ms: budgetMs }),
      },
      60_000
    );

  const init = (repoRoot: string) =>
    client.send<{ status: string }>({ request_id: `init-${requestId++}`, action: 'init', repo_root: repoRoot });

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-semantics-'));
    writeTree(root, {
      'tsconfig.json': TSCONFIG,
      'src/index.ts': 'export const service = 1;\n',
      'types/shims.d.ts': "declare module 'fixture-shorthand-http';\n",
      'node_modules/@fixture/http/package.json': packageJson('@fixture/http', '1.4.2', { types: 'index.d.ts' }),
      'node_modules/@fixture/http/index.d.ts': CONFIG_CLIENT,
      'node_modules/fixture-prefix-http/package.json': packageJson('fixture-prefix-http', '2.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-prefix-http/index.d.ts': PREFIX_CLIENT,
      [`node_modules/${W}/package.json`]: packageJson(W, '0.3.0', { types: 'index.d.ts' }),
      [`node_modules/${W}/index.d.ts`]: WRONG_CLIENT,
      'node_modules/fixture-broken-factory/package.json': packageJson('fixture-broken-factory', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-broken-factory/index.d.ts': BROKEN_FACTORY,
      'node_modules/fixture-overload-factory/package.json': packageJson('fixture-overload-factory', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-overload-factory/index.d.ts': OVERLOADED_FACTORY,
      'node_modules/fixture-any-http/package.json': packageJson('fixture-any-http', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-any-http/index.d.ts': ANY_CLIENT,
      'node_modules/fixture-js-http/package.json': packageJson('fixture-js-http', '3.1.0', { main: 'index.js' }),
      'node_modules/fixture-js-http/index.js': 'module.exports = { get() {} };\n',
    });
    // The same service with nothing installed.
    bare = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-semantics-bare-'));
    writeTree(bare, { 'tsconfig.json': TSCONFIG, 'src/index.ts': 'export const service = 1;\n' });

    client = new SidecarClient();
    await client.start();
    const ready = await init(root);
    assert.strictEqual(ready.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(bare, { recursive: true, force: true });
  });

  it('verifies every claim of both sample clients, on the export and on the instance', async () => {
    const checks = [...CONFIG_CHECKS, ...PREFIX_CHECKS];
    const response = await verify(root, checks);
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    const results = response.semantics!;
    // One result per check, in request order.
    assert.deepStrictEqual(pairs(results), pairs(checks));
    for (const result of results) {
      assert.deepStrictEqual(
        result,
        { claim_id: result.claim_id, receiver: result.receiver, verdict: 'verified' },
        `${result.claim_id} @ ${result.receiver}`
      );
      assert.ok(!('reason' in result), 'a verified result carries no reason');
    }
  });

  it('reads a named export the same way as the default one', async () => {
    const response = await verify(root, [check('@fixture/http', 'instance:create', VERB_GET, 'named')]);
    assert.strictEqual(response.semantics![0].verdict, 'verified');
  });

  it('fails each predicate with its reason when the declarations contradict the claim', async () => {
    const checks = WRONG_CASES.map(([claim]) => check(W, 'export', claim));
    const response = await verify(root, checks);
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    assert.deepStrictEqual(pairs(response.semantics!), pairs(checks));
    const got = response.semantics!.map(r => `${r.claim_id}: ${r.verdict} ${r.reason}`);
    const want = WRONG_CASES.map(([, verdict, reason], i) => `${checks[i].claim_id}: ${verdict} ${reason}`);
    assert.deepStrictEqual(got, want);
  });

  it('verifies nothing through an export typed any or unknown, or a shorthand module', async () => {
    const checks = [
      check('fixture-any-http', 'export', VERB_GET),
      check('fixture-any-http', 'instance:create', VERB_GET),
      check('fixture-any-http', 'export', VERB_GET, 'vague'),
      check('fixture-shorthand-http', 'export', VERB_GET),
    ];
    const response = await verify(root, checks);
    for (const result of response.semantics!) {
      assert.deepStrictEqual(
        [result.verdict, result.reason],
        ['unchecked', 'export_untyped'],
        `${result.claim_id} @ ${result.receiver}`
      );
    }
  });

  it('leaves a JS-only package, a missing package and a missing export unchecked', async () => {
    const response = await verify(root, [
      check('fixture-js-http', 'export', VERB_GET),
      check('fixture-not-installed', 'export', VERB_GET),
      check('@fixture/http', 'export', VERB_GET, 'absent'),
      // A type-only export has no value to call.
      check('@fixture/http', 'export', VERB_GET, 'RequestConfig'),
    ]);
    assert.deepStrictEqual(
      response.semantics!.map(r => [r.verdict, r.reason]),
      [
        ['unchecked', 'module_js_only'],
        ['unchecked', 'module_unresolved'],
        ['unchecked', 'export_missing'],
        ['unchecked', 'export_missing'],
      ]
    );
    const modules = new Map(response.semantics_modules!.map(m => [m.package, m]));
    assert.strictEqual(modules.get('fixture-js-http')!.reason, 'module_js_only');
    assert.match(modules.get('fixture-js-http')!.resolved_file!, /fixture-js-http\/index\.js$/);
    assert.strictEqual(modules.get('fixture-not-installed')!.reason, 'module_unresolved');
    const resolved = modules.get('@fixture/http')!;
    assert.strictEqual(resolved.reason, undefined);
    assert.strictEqual(resolved.installed_version, '1.4.2');
    assert.match(resolved.resolved_file!, /node_modules\/@fixture\/http\/index\.d\.ts$/);
  });

  it('leaves the instance unchecked when its factory does not resolve, while the export still verifies', async () => {
    const pkg = 'fixture-broken-factory';
    const response = await verify(root, [
      check(pkg, 'export', VERB_GET),
      check(pkg, 'instance:create', VERB_GET),
      check(pkg, 'instance:absent', VERB_GET),
      check(pkg, 'export', { kind: 'factory', member: 'create', base_url_key: 'baseURL' }),
    ]);
    assert.deepStrictEqual(
      response.semantics!.map(r => [r.receiver, r.verdict, r.reason]),
      [
        ['export', 'verified', undefined],
        ['instance:create', 'unchecked', 'factory_unresolved'],
        ['instance:absent', 'unchecked', 'factory_unresolved'],
        ['export', 'unchecked', 'factory_unresolved'],
      ]
    );
  });

  it('reads the instance from the overload that takes an options object, or the only one', async () => {
    const pkg = 'fixture-overload-factory';
    const response = await verify(root, [
      check(pkg, 'export', { kind: 'factory', member: 'create', base_url_key: 'baseURL' }),
      check(pkg, 'instance:create', VERB_GET),
      check(pkg, 'instance:make', VERB_GET),
      check(pkg, 'instance:solo', VERB_GET),
    ]);
    assert.deepStrictEqual(
      response.semantics!.map(r => [r.receiver, r.verdict, r.reason]),
      [
        ['export', 'verified', undefined],
        ['instance:create', 'verified', undefined],
        ['instance:make', 'unchecked', 'factory_unresolved'],
        ['instance:solo', 'verified', undefined],
      ]
    );
  });

  it('answers every check unchecked with reason budget when there is no time', async () => {
    const checks = CONFIG_CHECKS.slice(0, 5);
    const response = await verify(root, checks, 0);
    assert.strictEqual(response.status, 'success');
    assert.deepStrictEqual(pairs(response.semantics!), pairs(checks));
    for (const result of response.semantics!) {
      assert.deepStrictEqual([result.verdict, result.reason], ['unchecked', 'budget']);
    }
  });

  it('answers the same request the same way twice', async () => {
    const checks = [...CONFIG_CHECKS, ...WRONG_CASES.map(([claim]) => check(W, 'export', claim))];
    const first = await verify(root, checks);
    const second = await verify(root, checks);
    assert.deepStrictEqual(second.semantics, first.semantics);
    assert.deepStrictEqual(second.semantics_modules, first.semantics_modules);
  });

  it('refuses a receiver that is neither the export nor an instance', async () => {
    const bad = { ...check('@fixture/http', 'export', VERB_GET), receiver: 'create' };
    const response = await verify(root, [bad]);
    assert.strictEqual(response.status, 'error');
    assert.match(response.errors!.join(' '), /receiver/);
  });

  it('leaves every check unchecked on a service with no node_modules', async () => {
    const ready = await init(bare);
    assert.strictEqual(ready.status, 'ready');
    try {
      const checks = [...CONFIG_CHECKS.slice(0, 3), ...PREFIX_CHECKS.slice(0, 3)];
      const response = await verify(bare, checks);
      assert.deepStrictEqual(pairs(response.semantics!), pairs(checks));
      for (const result of response.semantics!) {
        assert.deepStrictEqual([result.verdict, result.reason], ['unchecked', 'module_unresolved']);
      }
      assert.deepStrictEqual(
        response.semantics_modules!.map(m => [m.package, m.reason]),
        [
          ['@fixture/http', 'module_unresolved'],
          ['fixture-prefix-http', 'module_unresolved'],
        ]
      );
    } finally {
      await init(root);
    }
  });
});

describe('verify_client_semantics before init', () => {
  it('needs init', async () => {
    const client = new SidecarClient();
    await client.start();
    try {
      const response = await client.send<Response>({
        request_id: 'uninit',
        action: 'verify_client_semantics',
        from_dir: os.tmpdir(),
        checks: [check('@fixture/http', 'export', VERB_GET)],
      });
      assert.strictEqual(response.status, 'error');
      assert.match(response.errors!.join(' '), /not initialized/);
    } finally {
      await client.stop();
    }
  });
});
