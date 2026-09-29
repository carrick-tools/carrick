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
  head(url: { href: string } | (string & {}), options?: Options): ResponsePromise;
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
  brand(url: string, options: string & { json?: unknown }): Promise<unknown>;
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

// A method key typed only by lower-case HTTP method literals.
const LOWER_CLIENT = `export interface Config {
  url: string;
  method: 'get' | 'post';
}
export interface Instance {
  request(config: Config): Promise<unknown>;
  post(url: string, body?: unknown): Promise<unknown>;
}
declare const client: Instance;
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
    { kind: 'request_body', member: 'request', args: 'config', url_key: 'url', method_key: 'method', body_key: 'data' },
    { kind: 'request_body', member: null, args: 'config', url_key: 'url', method_key: 'method', body_key: 'data' },
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
    // \`string & {}\` takes any string, as \`string\` does.
    { kind: 'verb', member: 'head', method: 'HEAD' },
    { kind: 'request', member: null, args: 'path_options', method_key: 'method' },
    { kind: 'request_body', member: null, args: 'path_options', method_key: 'method', body_key: 'json' },
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
  [{ kind: 'verb_body', member: 'absent', args: 'path_body' }, 'failed', 'member_missing'],
  [{ kind: 'verb_body', member: 'post', args: 'path_body' }, 'failed', 'param_missing'],
  [{ kind: 'verb_body', member: 'put', args: 'path_body' }, 'failed', 'body_not_open'],
  [{ kind: 'verb_body', member: 'patch', args: 'path_options' }, 'failed', 'options_not_object'],
  // A string has properties, but it is not an options object.
  [{ kind: 'verb_body', member: 'trace', args: 'path_options' }, 'failed', 'options_not_object'],
  // Nor is a string branded with an options-shaped object.
  [
    { kind: 'verb_body', member: 'brand', args: 'path_options', body_key: 'json' },
    'failed',
    'options_not_object',
  ],
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
  // A config claim that names no url key has nothing to find.
  [{ kind: 'request', member: 'submit', args: 'config', method_key: 'method' }, 'failed', 'key_missing'],
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
    { kind: 'request_body', member: 'submit', args: 'config', url_key: 'url', method_key: 'method', body_key: 'data' },
    'failed',
    'key_missing',
  ],
  [
    { kind: 'request_body', member: 'send', args: 'config', url_key: 'url', method_key: 'method', body_key: 'data' },
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
      // The service augments an installed package; the package is still the module.
      'src/augment.ts':
        "import '@fixture/http';\ndeclare module '@fixture/http' {\n  interface RequestConfig {\n    retry?: number;\n  }\n}\n",
      'node_modules/@fixture/http/package.json': packageJson('@fixture/http', '1.4.2', { types: 'index.d.ts' }),
      'node_modules/@fixture/http/index.d.ts': CONFIG_CLIENT,
      'node_modules/fixture-prefix-http/package.json': packageJson('fixture-prefix-http', '2.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-prefix-http/index.d.ts': PREFIX_CLIENT,
      [`node_modules/${W}/package.json`]: packageJson(W, '0.3.0', { types: 'index.d.ts' }),
      [`node_modules/${W}/index.d.ts`]: WRONG_CLIENT,
      'node_modules/fixture-broken-factory/package.json': packageJson('fixture-broken-factory', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-broken-factory/index.d.ts': BROKEN_FACTORY,
      'node_modules/fixture-lower-http/package.json': packageJson('fixture-lower-http', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/fixture-lower-http/index.d.ts': LOWER_CLIENT,
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

  it('verifies nothing through an export typed any or unknown', async () => {
    const checks = [
      check('fixture-any-http', 'export', VERB_GET),
      check('fixture-any-http', 'instance:create', VERB_GET),
      check('fixture-any-http', 'export', VERB_GET, 'vague'),
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

  it('verifies nothing through a shorthand module the service declares for itself', async () => {
    const response = await verify(root, [check('fixture-shorthand-http', 'export', VERB_GET)]);
    assert.deepStrictEqual(
      response.semantics!.map(r => [r.verdict, r.reason]),
      [['unchecked', 'module_local']]
    );
  });

  it('reads a body typed unknown as open', async () => {
    const response = await verify(root, [
      check('fixture-lower-http', 'export', { kind: 'verb_body', member: 'post', args: 'path_body' }),
    ]);
    assert.deepStrictEqual(response.semantics!.map(r => r.verdict), ['verified']);
  });

  it('accepts a method key typed only by lower-case HTTP method literals', async () => {
    const response = await verify(root, [
      check('fixture-lower-http', 'export', {
        kind: 'request',
        member: 'request',
        args: 'config',
        url_key: 'url',
        method_key: 'method',
      }),
    ]);
    assert.deepStrictEqual(response.semantics!.map(r => r.verdict), ['verified']);
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

  it('answers a receiver that is neither the export nor an instance on its own', async () => {
    const bad = { ...check('@fixture/http', 'export', VERB_GET), receiver: 'create' };
    const response = await verify(root, [bad, check('@fixture/http', 'export', VERB_GET)]);
    assert.strictEqual(response.status, 'success');
    assert.deepStrictEqual(
      response.semantics!.map(r => [r.receiver, r.verdict, r.reason]),
      [
        ['create', 'unchecked', 'receiver_invalid'],
        ['export', 'verified', undefined],
      ]
    );
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

/**
 * The review of PR #1567 wrote one invented package per way the declarations
 * can make a claim look true when they say nothing about it. Each case below
 * got `verified` before the fix; each asserts the verdict it gets now. A
 * verified claim becomes a fact that can fail a pull request check, so these
 * only ever move `verified` to `failed` or `unchecked`.
 */
describe('verify_client_semantics against declarations that say nothing (review of #1567)', () => {
  const ADV_TSCONFIG = JSON.stringify({
    compilerOptions: {
      target: 'es2020',
      module: 'commonjs',
      moduleResolution: 'node',
      strict: true,
      esModuleInterop: true,
      skipLibCheck: true,
      types: [],
      baseUrl: '.',
      paths: { 'adv-aliased': ['src/local-wrapper.ts'] },
    },
    include: ['src/**/*.ts', 'types/**/*.d.ts'],
  });
  const adv = (name: string, dts: string): Record<string, string> => ({
    [`node_modules/${name}/package.json`]: packageJson(name, '1.0.0', { types: 'index.d.ts' }),
    [`node_modules/${name}/index.d.ts`]: dts,
  });
  const FILES: Record<string, string> = {
    'tsconfig.json': ADV_TSCONFIG,
    'src/index.ts': 'export const service = 1;\n',
    // The service's own wrapper, reached through a `paths` alias under a package name.
    'src/local-wrapper.ts':
      "const w = { get(path: string): Promise<unknown> { return Promise.resolve(path); } };\nexport default w;\n",
    // A shim the service writes that shadows an installed package.
    'types/shim.d.ts':
      "declare module 'adv-shadowed' {\n  const c: { get(url: string): Promise<any>; post(url: string, body?: any): Promise<any> };\n  export default c;\n}\n",
    ...adv('adv-anyparams', 'export interface I { get(...args: any[]): any; post(...args: any[]): any }\ndeclare const c: I; export default c;\n'),
    ...adv('adv-peer', "import type { Url, Body } from 'adv-missing-peer';\nexport interface Inst { get(url: Url): Promise<unknown>; post(url: Url, body: Body): Promise<unknown>; request(config: { url: Url; method: Url }): Promise<unknown> }\nexport interface S extends Inst { create(o: { baseURL: Url }): Inst }\ndeclare const c: S; export default c;\n"),
    ...adv('adv-genopts', 'export interface Options { json?: unknown; headers?: Record<string, string> }\nexport interface I { post<O extends Options>(url: string, options?: O): Promise<unknown> }\ndeclare const c: I; export default c;\n'),
    ...adv('adv-defaultany', 'export interface Client<C = any> { get(url: C): Promise<unknown>; post(url: string, body: C): Promise<unknown>; create(o: { baseURL: C }): Client<C> }\ndeclare const c: Client; export default c;\n'),
    ...adv('adv-augment', 'export interface I { request(config: { url: string; method: string }): Promise<unknown>; post(url: string, options: { headers?: Record<string, string> }): Promise<unknown> }\ndeclare const c: I; export default c;\n'),
    ...adv('adv-splitsig', 'export interface I {\n  request(config: { url: string; method: string }): Promise<unknown>;\n  request(config: { data: unknown }): Promise<unknown>;\n}\ndeclare const c: I; export default c;\n'),
    ...adv('adv-tpreturn', 'export interface S { create<T = any>(o: { baseURL: string }): T; get(url: string): Promise<unknown> }\ndeclare const c: S; export default c;\n'),
    ...adv('adv-restparam', 'export interface I { post<A extends unknown[]>(url: string, ...rest: A): Promise<unknown> }\ndeclare const c: I; export default c;\n'),
    ...adv('adv-unicode', 'export interface I { poſt(url: string): Promise<unknown>; optıons(url: string): Promise<unknown> }\ndeclare const c: I; export default c;\n'),
    ...adv('adv-shadowed', 'export interface I { fetchIt(url: string): Promise<unknown> }\ndeclare const c: I; export default c;\n'),
    ...adv('adv-factorysplit', 'export interface Http { get(url: string): Promise<unknown> }\nexport interface Other { label: string }\nexport interface S {\n  create(o: { name: string }): Http;\n  create(o: { baseURL: string }): Other;\n}\ndeclare const c: S; export default c;\n'),
    ...adv('adv-loose', 'export interface I { get(url: {}): Promise<unknown>; request(config: { url: unknown; method: {} }): Promise<unknown>; create(o: { baseURL: unknown }): I }\ndeclare const c: I; export default c;\n'),
    ...adv('adv-cond', 'export interface Http { get(url: string): Promise<unknown> }\nexport interface S { create<O = any>(o: O & { baseURL?: string }): O extends { raw: true } ? any : Http }\ndeclare const c: S; export default c;\n'),
    ...adv('adv-indexany', 'export interface Http { get(url: string): Promise<unknown> }\ndeclare const c: Http & { [k: string]: any }; export default c;\n'),
    ...adv('adv-fnproto', 'declare function c(input: number): Promise<unknown>; export default c;\n'),
    ...adv('adv-mixed', 'export interface I { get(url: number): Promise<unknown>; get(url: any): Promise<unknown>; post: any }\ndeclare const c: I; export default c;\n'),
    ...adv('adv-object', 'export interface I { post(url: string, options: Object): Promise<unknown> }\ndeclare const c: I; export default c;\n'),
  };

  const FACTORY = (baseUrlKey: string): Claim => ({ kind: 'factory', member: 'create', base_url_key: baseUrlKey });
  const PATH_BODY = (member: string): Claim => ({ kind: 'verb_body', member, args: 'path_body' });
  const CONFIG_REQUEST: Claim = { kind: 'request', member: 'request', args: 'config', url_key: 'url', method_key: 'method' };
  const CONFIG_BODY = (member: string | null, bodyKey: string): Claim => ({
    kind: 'request_body',
    member,
    args: 'config',
    url_key: 'url',
    method_key: 'method',
    body_key: bodyKey,
  });

  /** [finding, what it shows, package, receiver, claim, verdict, reason] */
  const CASES: Array<[string, string, string, string, Claim, Result['verdict'], string]> = [
    ['A', 'a path typed any[] via a rest parameter', 'adv-anyparams', 'export', VERB_GET, 'unchecked', 'member_untyped'],
    ['A', 'a body behind a path typed any', 'adv-anyparams', 'export', PATH_BODY('post'), 'unchecked', 'member_untyped'],
    ['A', 'a path typed by a missing peer', 'adv-peer', 'export', VERB_GET, 'unchecked', 'member_untyped'],
    ['A', 'a body behind a path typed by a missing peer', 'adv-peer', 'export', PATH_BODY('post'), 'unchecked', 'member_untyped'],
    ['A', 'a base-URL key typed by a missing peer', 'adv-peer', 'export', FACTORY('baseURL'), 'unchecked', 'member_untyped'],
    ['A', 'request keys typed by a missing peer', 'adv-peer', 'export', CONFIG_REQUEST, 'unchecked', 'member_untyped'],
    ['A', 'a path typed by a type argument defaulted to any', 'adv-defaultany', 'export', VERB_GET, 'unchecked', 'member_untyped'],
    ['A/D', 'a body typed by a type argument defaulted to any', 'adv-defaultany', 'export', PATH_BODY('post'), 'unchecked', 'member_untyped'],
    ['A', 'a base-URL key typed by a type argument defaulted to any', 'adv-defaultany', 'export', FACTORY('baseURL'), 'unchecked', 'member_untyped'],
    ['A', 'an instance path typed by a type argument defaulted to any', 'adv-defaultany', 'instance:create', VERB_GET, 'unchecked', 'member_untyped'],
    ['A', 'a path typed {}', 'adv-loose', 'export', VERB_GET, 'unchecked', 'member_untyped'],
    ['A', 'request keys typed unknown and {}', 'adv-loose', 'export', CONFIG_REQUEST, 'unchecked', 'member_untyped'],
    ['A', 'a base-URL key typed unknown', 'adv-loose', 'export', FACTORY('baseURL'), 'unchecked', 'member_untyped'],
    ['B', 'a body key on another overload than the request keys', 'adv-splitsig', 'export', CONFIG_BODY('request', 'data'), 'failed', 'key_missing'],
    ['B', 'a body key on a number read as a config', 'adv-fnproto', 'export', CONFIG_BODY(null, 'toFixed'), 'failed', 'param_missing'],
    ['C', 'a body key every object inherits (constructor)', 'adv-augment', 'export', CONFIG_BODY('request', 'constructor'), 'failed', 'key_missing'],
    ['C', 'a body key every object inherits (toString)', 'adv-augment', 'export', CONFIG_BODY('request', 'toString'), 'failed', 'key_missing'],
    ['C', 'an options key every object inherits (valueOf)', 'adv-augment', 'export', { kind: 'verb_body', member: 'post', args: 'path_options', body_key: 'valueOf' }, 'failed', 'key_missing'],
    ['D', 'a body typed by a type parameter constrained to an options type', 'adv-genopts', 'export', PATH_BODY('post'), 'failed', 'body_not_open'],
    ['E', 'a service shim shadowing the installed package (verb)', 'adv-shadowed', 'export', VERB_GET, 'unchecked', 'module_local'],
    ['E', 'a service shim shadowing the installed package (body)', 'adv-shadowed', 'export', PATH_BODY('post'), 'unchecked', 'module_local'],
    ['E', 'a paths alias to the service\'s own wrapper', 'adv-aliased', 'export', VERB_GET, 'unchecked', 'module_local'],
    ['G', 'a factory returning an unconstrained type parameter', 'adv-tpreturn', 'export', FACTORY('baseURL'), 'unchecked', 'factory_unresolved'],
    ['G', 'a factory whose conditional return has an any branch', 'adv-cond', 'export', FACTORY('baseURL'), 'unchecked', 'factory_unresolved'],
    ['G', 'an instance from a conditional return with an any branch', 'adv-cond', 'instance:create', VERB_GET, 'unchecked', 'factory_unresolved'],
    ['H', 'a member that upper-cases to POST only through Unicode', 'adv-unicode', 'export', { kind: 'verb', member: 'poſt', method: 'POST' }, 'failed', 'method_not_member_verb'],
    ['H', 'a member that upper-cases to OPTIONS only through Unicode', 'adv-unicode', 'export', { kind: 'verb', member: 'optıons', method: 'OPTIONS' }, 'failed', 'method_not_member_verb'],
    ['I', 'an instance from another overload than the base-URL key', 'adv-factorysplit', 'instance:create', VERB_GET, 'unchecked', 'factory_unresolved'],
    ['J', 'a body read through a rest parameter typed by a type parameter', 'adv-restparam', 'export', PATH_BODY('post'), 'unchecked', 'member_untyped'],
    // Beyond the review's packages: the same definitions, other shapes.
    ['A', 'an overload typed any beside one that contradicts', 'adv-mixed', 'export', VERB_GET, 'unchecked', 'member_untyped'],
    ['A', 'a member typed any', 'adv-mixed', 'export', { kind: 'verb', member: 'post', method: 'POST' }, 'unchecked', 'member_untyped'],
    ['C', 'an options key only Object declares', 'adv-object', 'export', { kind: 'verb_body', member: 'post', args: 'path_options', body_key: 'valueOf' }, 'unchecked', 'member_untyped'],
  ];

  // True under the amended definitions; they must stay verified.
  const TRUE_CASES: Array<[string, string, string, Claim]> = [
    ['request keys on one overload', 'adv-splitsig', 'export', CONFIG_REQUEST],
    ['a json key on an options type parameter constrained to declare it', 'adv-genopts', 'export', { kind: 'verb_body', member: 'post', args: 'path_options', body_key: 'json' }],
    ['a factory overload declaring the base-URL key', 'adv-factorysplit', 'export', FACTORY('baseURL')],
    ['a declared verb beside an index signature typed any', 'adv-indexany', 'export', VERB_GET],
  ];

  let root: string;
  let client: SidecarClient;
  const results = new Map<string, Result>();
  const idOf = (index: number, pkg: string) => `adv-${index}:${pkg}`;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-semantics-adv-'));
    writeTree(root, FILES);
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'adv-init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
    const checks: Check[] = [
      ...CASES.map(([, , pkg, receiver, claim], i) => ({ ...check(pkg, receiver, claim), claim_id: idOf(i, pkg) })),
      ...TRUE_CASES.map(([, pkg, receiver, claim], i) => ({
        ...check(pkg, receiver, claim),
        claim_id: idOf(CASES.length + i, pkg),
      })),
    ];
    const response = await client.send<Response>(
      { request_id: 'adv', action: 'verify_client_semantics', from_dir: root, checks },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    assert.deepStrictEqual(pairs(response.semantics!), pairs(checks));
    for (const result of response.semantics!) results.set(result.claim_id, result);
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  CASES.forEach(([finding, shows, pkg, receiver, , verdict, reason], i) => {
    it(`${finding}: ${shows} is ${verdict} (${reason})`, () => {
      const result = results.get(idOf(i, pkg))!;
      assert.deepStrictEqual(
        [result.receiver, result.verdict, result.reason],
        [receiver, verdict, reason]
      );
    });
  });

  TRUE_CASES.forEach(([shows, pkg], i) => {
    it(`stays verified: ${shows}`, () => {
      const result = results.get(idOf(CASES.length + i, pkg))!;
      assert.deepStrictEqual([result.verdict, result.reason], ['verified', undefined]);
    });
  });

  it('reads resolved_file and installed_version from one answer', async () => {
    const response = await client.send<Response>({
      request_id: 'adv-modules',
      action: 'verify_client_semantics',
      from_dir: root,
      checks: [check('adv-shadowed', 'export', VERB_GET), check('adv-indexany', 'export', VERB_GET)],
    });
    const modules = new Map(response.semantics_modules!.map(m => [m.package, m]));
    const shadowed = modules.get('adv-shadowed')!;
    assert.match(shadowed.resolved_file!, /types\/shim\.d\.ts$/);
    assert.strictEqual(shadowed.installed_version, undefined);
    assert.strictEqual(shadowed.reason, 'module_local');
    const installed = modules.get('adv-indexany')!;
    assert.match(installed.resolved_file!, /node_modules\/adv-indexany\/index\.d\.ts$/);
    assert.strictEqual(installed.installed_version, '1.0.0');
  });
});

/**
 * The one place the check widens (carrick#1564, after the review of #1567):
 * a key is looked for through a type parameter's constraint, through a rest
 * parameter's element, and on the object constituents of a union, because
 * real client declarations take their defaults as `Options | (parent =>
 * Options)` or as a generic rest of instances and options. Everything else
 * here asserts the widening does NOT verify.
 */
describe('verify_client_semantics reads keys through unions, constraints and generic rests', () => {
  const FILES: Record<string, string> = {
    'tsconfig.json': TSCONFIG,
    'src/index.ts': 'export const service = 1;\n',
    'node_modules/fixture-extend-http/package.json': packageJson('fixture-extend-http', '1.0.0', { types: 'index.d.ts' }),
    'node_modules/fixture-extend-http/index.d.ts': `export interface Options {
  prefixUrl?: string;
  method?: string;
  json?: unknown;
}
export interface Instance {
  (url: string, options?: Options): Promise<unknown>;
  get(url: string, options?: Options): Promise<unknown>;
  head<U extends string>(url: U): Promise<unknown>;
  send(config: { url: string; method: string } | ((previous: Options) => void)): Promise<unknown>;
  extend(defaults: Options | ((parent: Options) => Options)): Instance;
  mixed(defaults: { prefixUrl: number } | { prefixUrl: string; retries?: number } | (() => void)): Instance;
  keyed<K extends string>(defaults: { prefixUrl: K }): Instance;
  call<M extends 'get' | 'post'>(config: { url: string; method: M }): Promise<unknown>;
  fetchLike(config: RequestInit & { url: string }): Promise<unknown>;
}
declare const client: Instance;
export default client;
`,
    'node_modules/fixture-merge-http/package.json': packageJson('fixture-merge-http', '1.0.0', { types: 'index.d.ts' }),
    'node_modules/fixture-merge-http/index.d.ts': `export interface Options {
  prefixUrl?: string;
}
export interface Instance {
  (url: string, options?: Options): Promise<unknown>;
  get(url: string, options?: Options): Promise<unknown>;
  extend<T extends Array<Instance | Options>>(...items: T): Instance;
}
declare const client: Instance;
export default client;
`,
    'node_modules/fixture-widen-wrong/package.json': packageJson('fixture-widen-wrong', '1.0.0', { types: 'index.d.ts' }),
    'node_modules/fixture-widen-wrong/index.d.ts': `export interface Options {
  prefixUrl?: string;
}
export interface Callable {
  (): void;
  prefixUrl: string;
}
export interface Instance {
  get(url: string): Promise<unknown>;
}
export interface Static {
  anyPart(defaults: Options | any): Instance;
  anyKey(defaults: { prefixUrl: any } | ((parent: Options) => Options)): Instance;
  indexOnly(defaults: { [key: string]: string } | ((parent: Options) => Options)): Instance;
  onFunction(defaults: Callable | number): Instance;
  anyConstraint<T extends any>(defaults: T): Instance;
  unknownElements<T extends unknown[]>(...items: T): Instance;
  numberKey(defaults: { prefixUrl: number } | ((parent: Options) => Options)): Instance;
}
declare const client: Static;
export default client;
`,
  };

  const factory = (member: string): Claim => ({ kind: 'factory', member, base_url_key: 'prefixUrl' });

  /** [what it shows, package, receiver, claim, verdict, reason] */
  const CASES: Array<[string, string, string, Claim, Result['verdict'], string | undefined]> = [
    ['a factory taking an options object or a function of the parent options', 'fixture-extend-http', 'export', factory('extend'), 'verified', undefined],
    ['a factory taking a generic rest of instances and options', 'fixture-merge-http', 'export', factory('extend'), 'verified', undefined],
    ['a path typed by a type parameter constrained to string', 'fixture-extend-http', 'export', { kind: 'verb', member: 'head', method: 'HEAD' }, 'verified', undefined],
    ['request keys on the object constituent of a union', 'fixture-extend-http', 'export', { kind: 'request', member: 'send', args: 'config', url_key: 'url', method_key: 'method' }, 'verified', undefined],
    ['a union where one object constituent types the key string and another number', 'fixture-extend-http', 'export', factory('mixed'), 'verified', undefined],
    ['a key typed by a type parameter constrained to string', 'fixture-extend-http', 'export', factory('keyed'), 'verified', undefined],
    ['a method key typed by a type parameter constrained to method literals', 'fixture-extend-http', 'export', { kind: 'request', member: 'call', args: 'config', url_key: 'url', method_key: 'method' }, 'verified', undefined],
    ['a method key the default library declares', 'fixture-extend-http', 'export', { kind: 'request', member: 'fetchLike', args: 'config', url_key: 'url', method_key: 'method' }, 'verified', undefined],
    ['a union whose key-bearing part collapses to any', 'fixture-widen-wrong', 'export', factory('anyPart'), 'unchecked', 'member_untyped'],
    ['a union whose only key-bearing constituent types the key any', 'fixture-widen-wrong', 'export', factory('anyKey'), 'unchecked', 'member_untyped'],
    ['a union where the key exists only through an index signature', 'fixture-widen-wrong', 'export', factory('indexOnly'), 'failed', 'key_missing'],
    ["a union where the key is a function constituent's own property", 'fixture-widen-wrong', 'export', factory('onFunction'), 'failed', 'key_missing'],
    ['a type parameter constrained to any', 'fixture-widen-wrong', 'export', factory('anyConstraint'), 'unchecked', 'member_untyped'],
    ['a generic rest whose element is unknown', 'fixture-widen-wrong', 'export', factory('unknownElements'), 'unchecked', 'member_untyped'],
    ['a union where the key is declared but typed number', 'fixture-widen-wrong', 'export', factory('numberKey'), 'failed', 'key_not_string'],
  ];

  let root: string;
  let client: SidecarClient;
  const results = new Map<string, Result>();

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-semantics-widen-'));
    writeTree(root, FILES);
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'widen-init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
    const checks = CASES.map(([, pkg, receiver, claim], i) => ({ ...check(pkg, receiver, claim), claim_id: `widen-${i}` }));
    const response = await client.send<Response>(
      { request_id: 'widen', action: 'verify_client_semantics', from_dir: root, checks },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    assert.deepStrictEqual(pairs(response.semantics!), pairs(checks));
    for (const result of response.semantics!) results.set(result.claim_id, result);
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  CASES.forEach(([shows, , , , verdict, reason], i) => {
    it(`${shows} is ${verdict}${reason ? ` (${reason})` : ''}`, () => {
      const result = results.get(`widen-${i}`)!;
      assert.deepStrictEqual([result.verdict, result.reason], [verdict, reason]);
    });
  });
});

/**
 * Re-review of #1567. Three ways a claim read true without any one installed
 * declaration saying so: a request's keys taken from different members of a
 * union config, a member the service's own module augmentation adds, and (the
 * other direction) an ordinary duplicate install that read `module_local`.
 */
describe('verify_client_semantics reads a config, a member and an install as one declaration (re-review of #1567)', () => {
  const RE_TSCONFIG = JSON.stringify({
    compilerOptions: {
      target: 'es2020',
      lib: ['es2020'],
      module: 'commonjs',
      moduleResolution: 'node',
      strict: true,
      esModuleInterop: true,
      skipLibCheck: true,
      types: [],
    },
    include: ['src/**/*.ts'],
  });
  const HTTP = `export interface Inst { get(url: string, config?: object): Promise<unknown>; post<D = unknown>(url: string, data?: D): Promise<unknown> }
export interface S extends Inst { create(config?: { baseURL?: string }): Inst }
declare const c: S; export default c;
`;
  const tail = 'declare const c: S; export default c;\n';
  let root: string;
  let client: SidecarClient;
  const results = new Map<string, Result>();
  let modules = new Map<string, Module>();

  const write = (rel: string, text: string) => writeTree(root, { [rel]: text });
  const pkgAt = (dir: string, name: string, dts: string, version = '1.0.0') =>
    writeTree(root, {
      [`${dir}/package.json`]: packageJson(name, version, { types: 'index.d.ts' }),
      [`${dir}/index.d.ts`]: dts,
    });

  const RQ = (member: string | null): Claim => ({ kind: 'request', member, args: 'config', url_key: 'url', method_key: 'method' });
  const RQB = (member: string | null, bodyKey: string): Claim => ({
    kind: 'request_body',
    member,
    args: 'config',
    url_key: 'url',
    method_key: 'method',
    body_key: bodyKey,
  });

  /** [item, what it shows, package, receiver, claim, verdict, reason] */
  const CASES: Array<[string, string, string, string, Claim, Result['verdict'], string | undefined]> = [
    ['1', 'url and method on different members of a discriminated union', 'wz-disc', 'export', RQ('request'), 'failed', 'key_missing'],
    ['1', 'url and method on different members of a union the receiver takes', 'wz-disc', 'export', RQ(null), 'failed', 'key_missing'],
    ['1', 'either/or members that each type the other key never', 'wz-xor', 'export', RQ('request'), 'failed', 'key_missing'],
    ['1', 'url and method on different members of a generic rest element', 'wz-nested-rest', 'export', RQ('request'), 'failed', 'key_missing'],
    ['1', 'url on one member and method on an intersection in a tuple rest', 'wz-nested-rest', 'export', RQ('send'), 'failed', 'key_missing'],
    ['1', 'url and method on different members of a labelled tuple rest', 'wz-tuple', 'export', RQ('fire'), 'failed', 'key_missing'],
    ['1', 'a string url only on the member without a method', 'wz-split-types', 'export', RQ('request'), 'failed', 'key_not_string'],
    ['1', 'url on a constrained type parameter and method on another member', 'wz-tp-member', 'export', RQ('request'), 'failed', 'key_missing'],
    ['1', 'a body key on another member than the url and method', 'wz-body-split', 'export', RQB('request', 'data'), 'failed', 'key_missing'],
    ['1', 'url, method and body key on one member of a union', 'wz-body-split', 'export', RQB('send', 'data'), 'verified', undefined],
    ['1', 'url and method on one member of a union', 'wz-body-split', 'export', RQ('send'), 'verified', undefined],
    ['1', 'a config union of function types only', 'wz-body-split', 'export', RQ('call'), 'failed', 'key_missing'],
    ['1', 'url and method only on a function member of a union', 'wz-body-split', 'export', RQ('fire'), 'failed', 'key_missing'],
    ['1', 'a factory key on the object member of options or a function', 'wz-extend', 'export', { kind: 'factory', member: 'extend', base_url_key: 'prefixUrl' }, 'verified', undefined],
    ['1', 'a factory key through a generic rest of instances and options', 'wz-extend', 'export', { kind: 'factory', member: 'create', base_url_key: 'prefixUrl' }, 'verified', undefined],
    ['2', 'a member only the service\'s own augmentation declares', 'rg-aug', 'export', { kind: 'verb_body', member: 'post', args: 'path_body' }, 'failed', 'member_missing'],
    ['2', 'a key only the service\'s own augmentation declares', 'rg-aug', 'export', { kind: 'factory', member: 'create', base_url_key: 'retryURL' }, 'failed', 'key_missing'],
    ['2', 'a member the installed package declares, beside the augmentation', 'rg-aug', 'export', VERB_GET, 'verified', undefined],
    ['2', 'a member an installed interface takes from a mapped type', 'rg-mapped', 'export', VERB_GET, 'verified', undefined],
    ['2', 'a mapped-type member the service\'s augmentation adds', 'rg-mapped-aug', 'export', { kind: 'verb', member: 'patch', method: 'PATCH' }, 'failed', 'member_missing'],
    // A client typed as an alias of an intersection: \`{ ... } & Record<Alias, Fn> & Fn\`.
    // An intersection has no symbol of its own; the member is listed by the
    // Record constituent, which the library wrote.
    ['2', 'a verb from a Record member of an intersection alias (default export)', 'rg-inter', 'export', VERB_GET, 'verified', undefined],
    ['2', 'a verb body from a Record member of an intersection alias', 'rg-inter', 'export', { kind: 'verb_body', member: 'post', args: 'path_options', body_key: 'json' }, 'verified', undefined],
    ['2', 'a verb from a Record member of an intersection alias (instance)', 'rg-inter', 'instance:extend', VERB_GET, 'verified', undefined],
    ['2', 'a factory on an intersection alias', 'rg-inter', 'export', { kind: 'factory', member: 'extend', base_url_key: 'prefixUrl' }, 'verified', undefined],
    ['2', 'a mapped member the service adds to a base inside an intersection alias', 'rg-inter-aug', 'export', { kind: 'verb', member: 'post', method: 'POST' }, 'failed', 'member_missing'],
    ['2', 'a library mapped member beside the service\'s addition to that base', 'rg-inter-aug', 'export', VERB_GET, 'verified', undefined],
    ['3', 'a duplicate install of the same name and version', 'rg-dup', 'export', VERB_GET, 'verified', undefined],
    ['3', 'a duplicate install whose first copy is a service-local file', 'rg-dup-local', 'export', VERB_GET, 'unchecked', 'module_local'],
    ['3', 'the same name declared by an installed copy at another version', 'rg-dup-version', 'export', VERB_GET, 'unchecked', 'module_local'],
  ];

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-semantics-re-'));
    write('tsconfig.json', RE_TSCONFIG);
    // Service code imports the nested copies first, so the top-level copy of
    // the same name and version becomes a redirect to the nested one.
    write('src/a.ts', "import dep from 'rg-dep'; import local from 'rg-dep-local'; import old from 'rg-dep-version'; export const d = [dep, local, old];\n");
    write('src/aug.ts', "import 'rg-aug';\ndeclare module 'rg-aug' {\n  interface S { post(url: string, body: unknown): Promise<unknown> }\n  interface Options { retryURL: string }\n}\nexport {};\n");
    // Item 1: request keys split across a union.
    pkgAt('node_modules/wz-disc', 'wz-disc', `export interface S { request(config: { kind: 'u'; url: string } | { kind: 'm'; method: string }): Promise<unknown>; (config: { url: string } | { method: 'GET' | 'POST' }): Promise<unknown> }\n${tail}`);
    pkgAt('node_modules/wz-xor', 'wz-xor', `export interface S { request(config: { url: string; method?: never } | { method: string; url?: never }): Promise<unknown> }\n${tail}`);
    pkgAt('node_modules/wz-nested-rest', 'wz-nested-rest', `export interface A { url: string } export interface B { method: string } export interface C { other: number }\nexport interface S { request<T extends Array<A | (B | C)>>(...args: T): Promise<unknown>; send<T extends [A | (B & C)]>(...args: T): Promise<unknown> }\n${tail}`);
    pkgAt('node_modules/wz-tuple', 'wz-tuple', `export interface S { fire(...a: [config: { url: string } | { method: string }]): Promise<unknown> }\n${tail}`);
    pkgAt('node_modules/wz-split-types', 'wz-split-types', `export interface S { request(config: { url: number; method: string } | { url: string; flag: true }): Promise<unknown> }\n${tail}`);
    pkgAt('node_modules/wz-tp-member', 'wz-tp-member', `export interface S { request<C extends { url: string }>(config: C | { method: string }): Promise<unknown> }\n${tail}`);
    pkgAt('node_modules/wz-body-split', 'wz-body-split', `export interface S { request(config: { url: string; method: string } | { data: unknown }): Promise<unknown>; send(config: { url: string; method: string; data?: unknown } | (() => void)): Promise<unknown>; call(config: (() => void) | ((n: number) => void)): Promise<unknown>; fire(config: { timeout?: number } | { (): void; url: string; method: string }): Promise<unknown> }\n${tail}`);
    pkgAt('node_modules/wz-extend', 'wz-extend', `export interface Options { prefixUrl?: string; method?: string; json?: unknown }\nexport interface Inst { (url: string, options?: Options): Promise<unknown>; get(url: string, options?: Options): Promise<unknown>; extend(defaults: Options | ((parent: Options) => Options)): Inst; create<T extends Array<Inst | Options>>(...items: T): Inst }\ndeclare const c: Inst; export default c;\n`);
    // Item 2: the installed package declares get and create; the service adds the rest.
    pkgAt('node_modules/rg-aug', 'rg-aug', `export interface Options { timeout?: number }\nexport interface S { get(url: string): Promise<unknown>; create(options: Options): S }\n${tail}`);
    // Members made by a mapped type carry no declaration of their own.
    pkgAt('node_modules/rg-mapped', 'rg-mapped', `export interface S extends Record<'get' | 'post', (url: string) => Promise<unknown>> { timeout: number }\n${tail}`);
    pkgAt('node_modules/rg-mapped-aug', 'rg-mapped-aug', `export interface S extends Record<'get' | 'post', (url: string) => Promise<unknown>> { timeout: number }\n${tail}`);
    write('src/aug-mapped.ts', "import 'rg-mapped-aug';\ndeclare module 'rg-mapped-aug' {\n  interface S extends Record<'patch', (url: string) => Promise<unknown>> {}\n}\nexport {};\n");
    const INTER = `export interface Options { prefixUrl?: string; url?: string; method?: string; json?: unknown }
export type Alias = 'get' | 'post' | 'put' | 'patch' | 'head' | 'delete';
export type RequestFn = {
  (url: string | { href: string }, options?: Options): Promise<unknown>;
  (options: Options & { url: string }): Promise<unknown>;
};
export type Client = { extend(...items: Array<Client | Options>): Client; defaults: Options } & Record<Alias, RequestFn> & RequestFn;
declare const client: Client;
export default client;
export { client };
`;
    pkgAt('node_modules/rg-inter', 'rg-inter', INTER);
    pkgAt('node_modules/rg-inter-aug', 'rg-inter-aug', `export interface Base { timeout?: number }
export type Client = Base & Record<'get', (url: string) => Promise<unknown>>;
declare const client: Client;
export default client;
`);
    write('src/aug-inter.ts', "import 'rg-inter-aug';\ndeclare module 'rg-inter-aug' {\n  interface Base extends Record<'post', (url: string) => Promise<unknown>> {}\n}\nexport {};\n");
    // Item 3: the same name and version twice, nested (loaded first) and at the top.
    pkgAt('node_modules/rg-dep', 'rg-dep', "import c from 'rg-dup';\ndeclare const d: typeof c; export default d;\n");
    pkgAt('node_modules/rg-dep/node_modules/rg-dup', 'rg-dup', HTTP);
    pkgAt('node_modules/rg-dup', 'rg-dup', HTTP);
    // ...where the first copy is a service-local directory reached through a symlink.
    pkgAt('node_modules/rg-dep-local', 'rg-dep-local', "import c from 'rg-dup-local';\ndeclare const d: typeof c; export default d;\n");
    pkgAt('vendor/rg-dup-local', 'rg-dup-local', HTTP);
    fs.mkdirSync(path.join(root, 'node_modules/rg-dep-local/node_modules'), { recursive: true });
    fs.symlinkSync(path.join(root, 'vendor/rg-dup-local'), path.join(root, 'node_modules/rg-dep-local/node_modules/rg-dup-local'));
    pkgAt('node_modules/rg-dup-local', 'rg-dup-local', HTTP);
    // ...and where an installed copy at another version declares the module itself.
    pkgAt('node_modules/rg-dep-version', 'rg-dep-version', "/// <reference path=\"./node_modules/rg-dup-version/index.d.ts\" />\nimport c from 'rg-dup-version';\ndeclare const d: typeof c; export default d;\n");
    pkgAt('node_modules/rg-dep-version/node_modules/rg-dup-version', 'rg-dup-version', `declare module 'rg-dup-version' {\n${HTTP}}\n`, '2.0.0');
    pkgAt('node_modules/rg-dup-version', 'rg-dup-version', HTTP);

    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 're-init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
    const checks = CASES.map(([, , pkg, receiver, claim], i) => ({ ...check(pkg, receiver, claim), claim_id: `re-${i}` }));
    const response = await client.send<Response>(
      { request_id: 're', action: 'verify_client_semantics', from_dir: root, checks },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    assert.deepStrictEqual(pairs(response.semantics!), pairs(checks));
    for (const result of response.semantics!) results.set(result.claim_id, result);
    modules = new Map(response.semantics_modules!.map(m => [m.package, m]));
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  CASES.forEach(([item, shows, , , , verdict, reason], i) => {
    it(`${item}: ${shows} is ${verdict}${reason ? ` (${reason})` : ''}`, () => {
      const result = results.get(`re-${i}`)!;
      assert.deepStrictEqual([result.verdict, result.reason], [verdict, reason]);
    });
  });

  it('3: reads the version of a duplicate install from the copy it resolves to', () => {
    const duplicate = modules.get('rg-dup')!;
    assert.strictEqual(duplicate.reason, undefined);
    assert.strictEqual(duplicate.installed_version, '1.0.0');
    assert.match(duplicate.resolved_file!, /node_modules\/rg-dep\/node_modules\/rg-dup\/index\.d\.ts$/);
  });
});

/**
 * Third review of #1567 (A1). A resolved file under `node_modules` is not
 * enough: a `paths` alias can point the requested name at ANOTHER installed
 * package. The package the resolver landed in must be the requested one (or
 * its separate `@types` package). The rest of this block pins the installs
 * that must still read as the named package.
 */
describe('verify_client_semantics requires the installed package to be the one named (third review of #1567)', () => {
  const HTTP = `export interface S { get(url: string): Promise<unknown>; create(o: { baseURL: string }): S }
declare const c: S; export default c;
`;
  let root: string;
  let client: SidecarClient;
  const results = new Map<string, Result>();
  let modules = new Map<string, Module>();
  const pkgAt = (dir: string, name: string, dts: string, entry: Record<string, unknown> = { types: 'index.d.ts' }, version = '1.0.0') =>
    writeTree(root, {
      [`${dir}/package.json`]: JSON.stringify({ name, version, ...entry }),
      [`${dir}/index.d.ts`]: dts,
    });

  const FACTORY: Claim = { kind: 'factory', member: 'create', base_url_key: 'baseURL' };
  /** [what it shows, package, claim, verdict, reason] */
  const CASES: Array<[string, string, Claim, Result['verdict'], string | undefined]> = [
    ['A1: a paths alias from the named package to another installed package (verb)', 'ax-named', VERB_GET, 'unchecked', 'module_local'],
    ['A1: a paths alias from the named package to another installed package (factory)', 'ax-named', FACTORY, 'unchecked', 'module_local'],
    ['A1: a paths alias to another installed package\'s directory', 'ax-named-dir', VERB_GET, 'unchecked', 'module_local'],
    ['a scoped package', '@fixture/scoped', VERB_GET, 'verified', undefined],
    ['a package typed through its separate @types package', 'fixture-typed', VERB_GET, 'verified', undefined],
    ['a scoped package typed through its mangled @types package', '@fixture/typed', VERB_GET, 'verified', undefined],
    ['a subpath the package exports', 'fixture-sub/client', VERB_GET, 'verified', undefined],
    ['a pnpm-style symlinked install', 'fixture-pnpm', VERB_GET, 'verified', undefined],
    ['a duplicate install of the same name and version', 'fixture-dup', VERB_GET, 'verified', undefined],
    // #1582: an npm alias installs the target under the alias's directory. The
    // directory after the last node_modules segment is the name asked for, and
    // its package is the one the resolver's packageId names.
    ['an npm alias whose installed package names itself otherwise', 'fixture-alias', VERB_GET, 'verified', undefined],
    ['a scoped npm alias', '@fixture/alias', VERB_GET, 'verified', undefined],
    // pnpm resolves an alias through the target's realpath, whose directory is
    // the target's name: this limit keeps it local.
    ['a pnpm npm alias (limit: the realpath names the target)', 'fixture-palias', VERB_GET, 'unchecked', 'module_local'],
    ['A1: a paths alias into a package nested inside the named package\'s directory', 'ax-holder', VERB_GET, 'unchecked', 'module_local'],
    ['A1: a paths alias to another package kept in a subdirectory of the named directory', 'fixture-shadow', VERB_GET, 'unchecked', 'module_local'],
    ['A1: a paths alias to a service-local copy whose package.json names the package', 'fixture-local', VERB_GET, 'unchecked', 'module_local'],
  ];

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-semantics-named-'));
    writeTree(root, {
      'tsconfig.json': JSON.stringify({
        compilerOptions: {
          target: 'es2020',
          lib: ['es2020'],
          module: 'esnext',
          moduleResolution: 'bundler',
          strict: true,
          esModuleInterop: true,
          skipLibCheck: true,
          types: [],
          baseUrl: '.',
          paths: {
            'ax-named': ['node_modules/ax-other/index.d.ts'],
            'ax-named-dir': ['node_modules/ax-other'],
            'ax-holder': ['node_modules/ax-holder/node_modules/ax-inner'],
            'fixture-shadow': ['node_modules/fixture-shadow/lib/other'],
            'fixture-local': ['vendor/fixture-local'],
          },
        },
        include: ['src/**/*.ts'],
      }),
      // The nested copy of the duplicate is imported first, so the top-level one becomes a redirect to it.
      'src/index.ts': "import dep from 'fixture-dep';\nexport const service = dep;\n",
    });
    pkgAt('node_modules/ax-other', 'ax-other', HTTP);
    pkgAt('node_modules/ax-named', 'ax-named', 'export interface S { fetchIt(u: number): void }\ndeclare const c: S; export default c;\n');
    pkgAt('node_modules/@fixture/scoped', '@fixture/scoped', HTTP);
    const jsOnly = (dir: string, name: string) =>
      writeTree(root, {
        [`${dir}/package.json`]: JSON.stringify({ name, version: '1.0.0', main: 'index.js' }),
        [`${dir}/index.js`]: 'module.exports = {};\n',
      });
    jsOnly('node_modules/fixture-typed', 'fixture-typed');
    pkgAt('node_modules/@types/fixture-typed', '@types/fixture-typed', HTTP, { types: 'index.d.ts' }, '2.0.1');
    jsOnly('node_modules/@fixture/typed', '@fixture/typed');
    pkgAt('node_modules/@types/fixture__typed', '@types/fixture__typed', HTTP, { types: 'index.d.ts' }, '3.0.2');
    writeTree(root, {
      'node_modules/fixture-sub/package.json': JSON.stringify({
        name: 'fixture-sub',
        version: '1.0.0',
        exports: { './client': { types: './dist/client.d.ts' } },
      }),
      'node_modules/fixture-sub/dist/client.d.ts': HTTP,
    });
    pkgAt('node_modules/.pnpm/fixture-pnpm@1.0.0/node_modules/fixture-pnpm', 'fixture-pnpm', HTTP);
    fs.symlinkSync(
      path.join(root, 'node_modules/.pnpm/fixture-pnpm@1.0.0/node_modules/fixture-pnpm'),
      path.join(root, 'node_modules/fixture-pnpm')
    );
    pkgAt('node_modules/fixture-dep', 'fixture-dep', "import c from 'fixture-dup';\ndeclare const d: typeof c; export default d;\n");
    pkgAt('node_modules/fixture-dep/node_modules/fixture-dup', 'fixture-dup', HTTP);
    pkgAt('node_modules/fixture-dup', 'fixture-dup', HTTP);
    // What `"fixture-alias": "npm:fixture-target@1"` installs: the target, under the alias's directory.
    pkgAt('node_modules/fixture-alias', 'fixture-target', HTTP);
    pkgAt('node_modules/@fixture/alias', 'fixture-target', HTTP);
    pkgAt('node_modules/.pnpm/fixture-ptarget@1.0.0/node_modules/fixture-ptarget', 'fixture-ptarget', HTTP);
    fs.symlinkSync(
      path.join(root, 'node_modules/.pnpm/fixture-ptarget@1.0.0/node_modules/fixture-ptarget'),
      path.join(root, 'node_modules/fixture-palias')
    );
    pkgAt('node_modules/ax-holder', 'ax-holder', 'export interface S { fetchIt(u: number): void }\ndeclare const c: S; export default c;\n');
    pkgAt('node_modules/ax-holder/node_modules/ax-inner', 'ax-inner', HTTP);
    pkgAt('node_modules/fixture-shadow', 'fixture-shadow', 'export interface S { fetchIt(u: number): void }\ndeclare const c: S; export default c;\n');
    pkgAt('node_modules/fixture-shadow/lib/other', 'ax-other', HTTP);
    pkgAt('vendor/fixture-local', 'fixture-local', HTTP);

    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'named-init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
    const checks = CASES.map(([, pkg, claim], i) => ({ ...check(pkg, 'export', claim), claim_id: `named-${i}` }));
    const response = await client.send<Response>(
      { request_id: 'named', action: 'verify_client_semantics', from_dir: root, checks },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    assert.deepStrictEqual(pairs(response.semantics!), pairs(checks));
    for (const result of response.semantics!) results.set(result.claim_id, result);
    modules = new Map(response.semantics_modules!.map(m => [m.package, m]));
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  CASES.forEach(([shows, , , verdict, reason], i) => {
    it(`${shows} is ${verdict}${reason ? ` (${reason})` : ''}`, () => {
      const result = results.get(`named-${i}`)!;
      assert.deepStrictEqual([result.verdict, result.reason], [verdict, reason]);
    });
  });

  it('reads installed_version from the named package, @types included', () => {
    assert.strictEqual(modules.get('ax-named')!.installed_version, undefined);
    // The directory alias resolves with the other package's id; its version is not the named one's.
    assert.strictEqual(modules.get('ax-named-dir')!.installed_version, undefined);
    assert.strictEqual(modules.get('@fixture/typed')!.installed_version, '3.0.2');
    assert.strictEqual(modules.get('fixture-sub/client')!.installed_version, '1.0.0');
  });
});

/**
 * #1582: declarations the service writes for itself. The whole tree sits under
 * an ancestor directory named `node_modules` and the service is a package of a
 * monorepo, so every row here also pins what "installed" means: a package
 * directory after the last `node_modules` segment of the path relative to the
 * service root.
 */
describe('verify_client_semantics ignores what the service declares for a package (#1582)', () => {
  let base: string;
  let repo: string;
  let svc: string;
  let link: string;
  let alias: string;
  let client: SidecarClient;
  const results = new Map<string, Result>();
  const tail = 'declare const c: S; export default c;\n';
  const write = (rel: string, text: string) => writeTree(svc, { [rel]: text });
  const pkgIn = (dir: string, name: string, dts: string) =>
    writeTree(dir, {
      [`node_modules/${name}/package.json`]: packageJson(name, '1.0.0', { types: 'index.d.ts' }),
      [`node_modules/${name}/index.d.ts`]: dts,
    });

  const PB = (member: string): Claim => ({ kind: 'verb_body', member, args: 'path_body' });
  const VERB = (member: string): Claim => ({ kind: 'verb', member, method: member.toUpperCase() });
  /** [finding, what it shows, package, claim, verdict, reason] */
  const CASES: Array<[string, string, string, Claim, Result['verdict'], string | undefined]> = [
    ['B1', 'a string overload the service adds to a member the library types number', 'ax-over', VERB_GET, 'failed', 'path_not_string'],
    ['B1', 'a call signature the service adds to the callable export, over a library config type', 'ax-callable', { kind: 'request', member: null, args: 'config', url_key: 'url', method_key: 'method' }, 'failed', 'param_missing'],
    ['B2', 'a member added by a service file in a source directory named node_modules (verb)', 'ax-aug3', VERB('post'), 'failed', 'member_missing'],
    ['B2', 'a member added by a service file in a source directory named node_modules (body)', 'ax-aug3', PB('post'), 'failed', 'member_missing'],
    ['B2', 'a member the service adds when its repository sits under a node_modules ancestor', 'ax-anc', VERB('post'), 'failed', 'member_missing'],
    ['B2', 'a member added by a service file in a node_modules directory with no package.json', 'ax-aug5', VERB('post'), 'failed', 'member_missing'],
    ['B2', 'a package installed in the service', 'ax-anc', VERB_GET, 'verified', undefined],
    ['B2', 'a package hoisted to the monorepo root above the service', 'fixture-hoisted', VERB_GET, 'verified', undefined],
    ['B2', 'a pnpm-style symlinked install', 'fixture-pnpm2', VERB_GET, 'verified', undefined],
    ['B2', 'a method key the default library declares', 'fixture-lib', { kind: 'request', member: 'send', args: 'config', url_key: 'url', method_key: 'method' }, 'verified', undefined],
    ['B3', 'a mapped member the service adds to a library base interface (verb)', 'ax-base', VERB('post'), 'failed', 'member_missing'],
    ['B3', 'a mapped member the service adds to a library base interface (body)', 'ax-base', PB('post'), 'failed', 'member_missing'],
    ['B3', 'a mapped member the service adds two bases down', 'ax-deep', VERB('post'), 'failed', 'member_missing'],
    ['B3', 'a mapped member the service adds to the base of a generic interface', 'ax-generic', VERB('post'), 'failed', 'member_missing'],
    ['B3', 'the library member beside the service\'s addition to the base', 'ax-base', VERB_GET, 'verified', undefined],
    ['B3', 'verbs an installed interface takes from a Record base', 'fixture-iface', VERB_GET, 'verified', undefined],
    ['B3', 'verbs an installed intersection alias takes from a Record part', 'fixture-inter', VERB_GET, 'verified', undefined],
    ['B3', 'a mapped member reached through a base another library base also lists', 'ax-twobase', VERB('post'), 'verified', undefined],
  ];

  before(async () => {
    base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-semantics-1582-'));
    repo = path.join(base, 'real', 'node_modules', 'work', 'repo');
    svc = path.join(repo, 'packages', 'svc');
    link = path.join(base, 'svc-link');
    alias = path.join(base, 'alias');
    writeTree(svc, {
      'tsconfig.json': JSON.stringify({
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
      }),
      'src/index.ts': "import './node_modules/aug';\nimport './node_modules/plugins/aug5';\nexport const service = 1;\n",
      'src/node_modules/plugins/aug5.ts': "import 'ax-aug5';\ndeclare module 'ax-aug5' { interface S { post(url: string): Promise<unknown> } }\nexport {};\n",
      // B2: a service source file in a directory that happens to be named node_modules.
      'src/node_modules/aug.ts': "import 'ax-aug3';\ndeclare module 'ax-aug3' { interface S { post(url: string, body: unknown): Promise<unknown> } }\nexport {};\n",
      // B1: overloads the service adds.
      'src/over.ts': "import 'ax-over';\nimport 'ax-callable';\ndeclare module 'ax-over' { interface S { get(url: string): Promise<unknown> } }\ndeclare module 'ax-callable' { interface S { (config: Config): Promise<unknown> } }\nexport {};\n",
      // B2: under a node_modules ancestor, the service's own file still is not installed.
      'src/anc.ts': "import 'ax-anc';\ndeclare module 'ax-anc' { interface S { post(url: string): Promise<unknown> } }\nexport {};\n",
      // B3: the service extends a library base interface with its own mapped type.
      'src/generic.ts': "import 'ax-generic';\ndeclare module 'ax-generic' { interface Base<T> extends Record<'post', (url: string) => Promise<unknown>> {} }\nexport {};\n",
      'src/base.ts': "import 'ax-base';\nimport 'ax-deep';\nimport 'ax-twobase';\ndeclare module 'ax-base' { interface Base extends Record<'post', (url: string, body: unknown) => Promise<unknown>> {} }\ndeclare module 'ax-deep' { interface Root extends Record<'post', (url: string) => Promise<unknown>> {} }\ndeclare module 'ax-twobase' { interface Extra extends Record<'post', (url: string) => Promise<unknown>> {} }\nexport {};\n",
    });
    pkgIn(svc, 'ax-over', `export interface S { get(url: number): Promise<unknown> }\n${tail}`);
    pkgIn(svc, 'ax-callable', `export interface Config { url: string; method: string }\nexport interface S { (config: number): Promise<unknown> }\n${tail}`);
    pkgIn(svc, 'ax-aug3', `export interface S { get(url: string): Promise<unknown> }\n${tail}`);
    pkgIn(svc, 'ax-anc', `export interface S { get(url: string): Promise<unknown> }\n${tail}`);
    pkgIn(svc, 'ax-aug5', `export interface S { get(url: string): Promise<unknown> }\n${tail}`);
    // The ancestor named node_modules is itself a package directory. Two
    // symlinks reach the service: one to its root, one to the tree above the
    // ancestor, so file names can carry `node_modules/work` without being real.
    writeTree(base, { 'real/node_modules/work/package.json': packageJson('work', '1.0.0', {}) });
    fs.symlinkSync(svc, link);
    fs.symlinkSync(path.join(base, 'real'), alias);
    pkgIn(repo, 'fixture-hoisted', `export interface S { get(url: string): Promise<unknown> }\n${tail}`);
    writeTree(svc, {
      'node_modules/.pnpm/fixture-pnpm2@1.0.0/node_modules/fixture-pnpm2/package.json': packageJson('fixture-pnpm2', '1.0.0', { types: 'index.d.ts' }),
      'node_modules/.pnpm/fixture-pnpm2@1.0.0/node_modules/fixture-pnpm2/index.d.ts': `export interface S { get(url: string): Promise<unknown> }\n${tail}`,
    });
    fs.symlinkSync(
      path.join(svc, 'node_modules/.pnpm/fixture-pnpm2@1.0.0/node_modules/fixture-pnpm2'),
      path.join(svc, 'node_modules/fixture-pnpm2')
    );
    pkgIn(svc, 'fixture-lib', `export interface S { send(config: RequestInit & { url: string }): Promise<unknown> }\n${tail}`);
    pkgIn(svc, 'ax-base', `export interface Base { get(url: string): Promise<unknown> }\nexport interface S extends Base {}\n${tail}`);
    pkgIn(svc, 'ax-deep', `export interface Root { get(url: string): Promise<unknown> }\nexport interface Mid extends Root {}\nexport interface S extends Mid {}\n${tail}`);
    pkgIn(svc, 'ax-generic', `export interface Base<T> { get(url: T): Promise<unknown> }\nexport interface S<T = string> extends Base<T> {}\n${tail}`);
    pkgIn(svc, 'fixture-iface', `export interface S extends Record<'get' | 'post', (url: string) => Promise<unknown>> { timeout: number }\n${tail}`);
    pkgIn(svc, 'fixture-inter', `export type S = { timeout: number } & Record<'get' | 'post', (url: string) => Promise<unknown>>;\n${tail}`);
    pkgIn(svc, 'ax-twobase', `export interface Lib extends Record<'post', (url: string) => Promise<unknown>> {}\nexport interface Extra {}\nexport interface S extends Lib, Extra {}\n${tail}`);

    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'b-init', action: 'init', repo_root: svc });
    assert.strictEqual(ready.status, 'ready');
    const checks = CASES.map(([, , pkg, claim], i) => ({ ...check(pkg, 'export', claim), claim_id: `b-${i}` }));
    const response = await client.send<Response>(
      { request_id: 'b', action: 'verify_client_semantics', from_dir: svc, checks },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    assert.deepStrictEqual(pairs(response.semantics!), pairs(checks));
    for (const result of response.semantics!) results.set(result.claim_id, result);
  });

  after(async () => {
    await client.stop();
    fs.rmSync(base, { recursive: true, force: true });
  });

  CASES.forEach(([finding, shows, , , verdict, reason], i) => {
    it(`${finding}: ${shows} is ${verdict}${reason ? ` (${reason})` : ''}`, () => {
      const result = results.get(`b-${i}`)!;
      assert.deepStrictEqual([result.verdict, result.reason], [verdict, reason]);
    });
  });

  const ANCESTOR_CHECKS = () => [check('ax-anc', 'export', VERB('post')), check('ax-anc', 'export', VERB_GET)];
  const ANCESTOR_VERDICTS = [
    ['failed', 'member_missing'],
    ['verified', undefined],
  ];

  it('B2: judges files against the realpath of a service root reached through a symlink', async () => {
    const response = await client.send<Response>({
      request_id: 'b-link',
      action: 'verify_client_semantics',
      from_dir: link,
      checks: ANCESTOR_CHECKS(),
    });
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    assert.deepStrictEqual(response.semantics!.map(r => [r.verdict, r.reason]), ANCESTOR_VERDICTS);
  });

  it('B2: judges files named through a symlinked path by their realpath', async () => {
    const through = path.join(alias, 'node_modules', 'work', 'repo', 'packages', 'svc');
    const other = new SidecarClient();
    await other.start();
    try {
      const ready = await other.send<{ status: string }>({ request_id: 'b-alias-init', action: 'init', repo_root: through });
      assert.strictEqual(ready.status, 'ready');
      const response = await other.send<Response>(
        { request_id: 'b-alias', action: 'verify_client_semantics', from_dir: through, checks: ANCESTOR_CHECKS() },
        60_000
      );
      assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
      assert.deepStrictEqual(response.semantics!.map(r => [r.verdict, r.reason]), ANCESTOR_VERDICTS);
    } finally {
      await other.stop();
    }
  });
});
