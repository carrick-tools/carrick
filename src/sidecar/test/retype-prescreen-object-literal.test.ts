/**
 * carrick#3158: the retype's "the call's type argument does not reach its
 * result" is decided before the rewrite when the type argument the rewrite
 * fills appears in the result only as a conditional's check or extends type
 * (or not at all). The sidecar answers `fresh_type_reason`; the scanner
 * applies it only when the producer's type is one new object type.
 *
 * The rule is the proof's input, so the cases below pin both sides: a to c
 * are decided, d to j look alike and must NOT be (each breaks one condition),
 * and the equivalence tests pin that, for the producer types the scanner
 * applies the reason to, the full retype answers the same string.
 *
 * The declarations are synthetic: a generated-client shape whose only type
 * parameter is a throw-on-error switch, and plain generic helpers.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SDK = `export interface Opts<TBody = unknown, Throw extends boolean = false> {
  body?: TBody;
  throwOnError?: Throw;
}

export type Result<TData, TError, Throw extends boolean = boolean, S = 'fields'> =
  Throw extends true
    ? Promise<S extends 'fields' ? { data: TData; request: Request; response: Response } : TData>
    : Promise<
        ({ data: TData; error: undefined } | { data: undefined; error: TError }) & {
          request?: Request;
          response?: Response;
        }
      >;

export type ThingResponse = { id: string; n: number };
export type FlagResponse = true;
export type Failure = { message: string };

export declare const getThing: <Throw extends boolean = false>(
  options: Opts<unknown, Throw>
) => Result<ThingResponse, Failure, Throw>;

export declare const getFlag: <Throw extends boolean = false>(
  options: Opts<unknown, Throw>
) => Result<FlagResponse, Failure, Throw>;

export declare function submit<D>(url: string, data: D): Promise<{ ok: boolean }>;
export declare function fetchJson<T>(url: string): Promise<T>;
export declare function selectOnly<B extends boolean = false>(): Promise<B>;
export declare function viaDefault<B extends boolean = false, R = B>(): Promise<R>;

export type Sel3<B, C> = [B, C] extends [true, true] ? { a: string } : { b: string };
export declare function sel3<B, C>(): Sel3<B, C>;

export declare function awaitedOf<T>(url: string): Promise<Awaited<T>>;
export declare function indexed<T>(url: string): Promise<T[keyof T]>;

export declare function overloaded<B extends boolean = false>(
  url: string
): Promise<B extends true ? { x: 1 } : { y: 2 }>;
export declare function overloaded<B extends boolean = false>(
  url: number
): Promise<B extends true ? { x: 1 } : { y: 2 }>;
`;

// One call per case; each function declares its return type and reads the
// result, so the retype's own pre-rewrite checks pass for all ten.
const CONSUMER = `import {
  getThing, getFlag, submit, fetchJson, selectOnly, viaDefault, sel3, awaitedOf, indexed, overloaded,
} from './sdk';

export async function a(): Promise<number> {
  const r = await getThing({});
  return r.data ? r.data.n : 0;
}

export async function b(): Promise<boolean> {
  const r = await getFlag({});
  return r.data === true;
}

export async function c(): Promise<boolean> {
  const r = await submit('/p', { a: 1 });
  return r.ok;
}

export async function d(): Promise<string> {
  const r = await fetchJson('/x');
  return String(r);
}

export async function e(): Promise<string> {
  const r = await selectOnly();
  return String(r);
}

export async function f(): Promise<string> {
  const r = await viaDefault();
  return String(r);
}

export function g<Q>(): string {
  const r = sel3<false, Q>();
  return String(r);
}

export async function h(): Promise<string> {
  const r = await awaitedOf('/x');
  return String(r);
}

export async function i(): Promise<string> {
  const r = await indexed('/x');
  return String(r);
}

export async function j(): Promise<string> {
  const r = await overloaded('/x');
  return String(r);
}

// The same call as a, inside a generic function: a producer type may name Q.
export async function k<Q>(q: Q): Promise<number> {
  const r = await getThing({});
  return r.data ? r.data.n : q ? 1 : 0;
}
`;

const CALLS = {
  a: "getThing({})",
  b: "getFlag({})",
  c: "submit('/p', { a: 1 })",
  d: "fetchJson('/x')",
  e: "selectOnly()",
  f: "viaDefault()",
  g: "sel3<false, Q>()",
  h: "awaitedOf('/x')",
  i: "indexed('/x')",
  j: "overloaded('/x')",
  k: "getThing({})",
} as const;
type Name = keyof typeof CALLS;
const NAMES = Object.keys(CALLS) as Name[];

/** The 1-based line of the one CONSUMER line that holds `text`. */
const lineOf = (text: string, nth = 0): number =>
  CONSUMER.split('\n')
    .map((line, index) => (line.includes(text) ? index + 1 : 0))
    .filter(Boolean)[nth];

interface Outcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'abstain' | 'wider';
  reason?: string;
}

describe('carrick#3158: the retype decides "does not reach" before the rewrite', () => {
  let client: SidecarClient;
  let repoDir: string;
  let consumerPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3158-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'sdk.ts'), SDK);
    consumerPath = path.join(repoDir, 'src', 'consumer.ts');
    fs.writeFileSync(consumerPath, CONSUMER);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const locator = (name: Name) => ({
    file_path: consumerPath,
    line_number: lineOf(CALLS[name], name === 'k' ? 1 : 0),
    expression_text: CALLS[name],
    expression_line: lineOf(CALLS[name], name === 'k' ? 1 : 0),
  });

  async function prescreen(
    names: Name[]
  ): Promise<Map<string, { reason?: string; fresh_type_reason?: string }>> {
    const res = await client.send<{
      status: string;
      prescreens?: Array<{ item_id: string; reason?: string; fresh_type_reason?: string }>;
    }>({
      action: 'retype_prescreen',
      request_id: 'prescreen',
      items: names.map((name) => ({ item_id: name, ...locator(name) })),
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    return new Map(res.prescreens!.map((p) => [p.item_id, p]));
  }

  async function retype(name: Name, producerType: string): Promise<Outcome> {
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
      action: 'retype_check',
      request_id: `${name}-${producerType}`,
      items: [{ item_id: name, ...locator(name), producer_type: producerType, wire: true }],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    return res.outcomes![0];
  }

  it('leaves every case to the rewrite as far as `reason` goes', async () => {
    const answers = await prescreen(NAMES);
    for (const name of NAMES) {
      assert.strictEqual(answers.get(name)?.reason, undefined, name);
    }
  });

  it('answers fresh_type_reason for a selector-only type argument and for no other shape', async () => {
    const answers = await prescreen(NAMES);
    const decided = NAMES.filter((name) => answers.get(name)?.fresh_type_reason !== undefined);
    // a: a throw switch; b: the same with a `true` data type; c: a type
    // parameter the result never names.
    assert.deepStrictEqual(decided, ['a', 'b', 'c']);
    // d: the result IS the parameter. e: the same through a selector default.
    // f: a default that names a type parameter. g: a free type parameter
    // among the written arguments keeps the conditional deferred. h: a
    // library alias that returns its argument. i: an indexed access.
    // j: two signatures. k: inside a generic function.
  });

  it('words the reason as the full retype does, quoting the call\'s own result', async () => {
    const answers = await prescreen(['a', 'b', 'c']);
    assert.match(
      answers.get('a')!.fresh_type_reason!,
      /^the call's type argument does not reach its result \('.+'\), so it does not state the response$/
    );
  });

  // The proof's check: for the producer types the scanner applies the
  // reason to, the full retype gives the same abstention, string for string.
  const FRESH = [
    '{ id: string; n: number; }',
    '{ ok: boolean; }',
    '{ x: number; }[]',
    '{ data: { id: string; n: number; }; }',
  ];
  for (const name of ['a', 'b', 'c'] as const) {
    for (const producer of FRESH) {
      it(`(${name}) the full retype answers the prescreen's reason for ${producer}`, async () => {
        const reason = (await prescreen([name])).get(name)!.fresh_type_reason;
        assert.ok(reason, name);
        const out = await retype(name, producer);
        assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
        assert.strictEqual(out.reason, reason);
      });
    }
  }

  // The boundary the scanner's gate keeps: these producer types reach a slot
  // on the same call, so the reason is NOT theirs.
  it('a `true` producer reaches the data of a `true`-data call and is judged', async () => {
    const out = await retype('b', 'true');
    assert.notStrictEqual(out.outcome, 'abstain', JSON.stringify(out));
  });

  it('a `boolean` producer reaches the `ok` of a body-generic call and is judged', async () => {
    const out = await retype('c', 'boolean');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
  });
});
