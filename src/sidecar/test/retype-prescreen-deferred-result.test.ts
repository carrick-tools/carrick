/**
 * carrick#3165: tier 2 of the early "does not reach" answer. A generated
 * client's method call states the response map as its first type argument and
 * its own free `Throw` type parameter as a companion, so the call's result is
 * `Awaited` of a conditional that stays deferred. Every member the retype's
 * reach test reads holds an `undefined` that the stated type never put there,
 * so the stated type reaches it only when it has a top-level `undefined`.
 *
 * The sidecar answers `undefined_free_reason` for such a call; the scanner
 * applies it only when the producer's text has no top-level `undefined`.
 *
 * The cases pin both sides. P1 and P1b are decided. Every other call looks
 * alike and must NOT be decided, each breaking one condition of the rule
 * (the full retype reaches a slot for N1, N2, N6 and N8, so those guards are
 * load-bearing). The equivalence tests pin that, for the producer types the
 * scanner applies the reason to, the full retype abstains with the same
 * sentence.
 *
 * The declarations are synthetic: a generated-client shape under neutral
 * names, run in a strict and in a loose program.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const CLIENT = `export type Style = 'data' | 'fields';

export type Pick1<T> = T extends Record<string, unknown> ? T[keyof T] : T;

export type CallResult<
  TData = unknown,
  TError = unknown,
  Throw extends boolean = boolean,
  TStyle extends Style = 'fields',
> = Throw extends true
  ? Promise<
      TStyle extends 'data'
        ? Pick1<TData>
        : { data: Pick1<TData>; request: Request; response: Response }
    >
  : Promise<
      TStyle extends 'data'
        ? Pick1<TData> | undefined
        : ({ data: Pick1<TData>; error: undefined } | { data: undefined; error: Pick1<TError> }) & {
            request?: Request;
            response?: Response;
          }
    >;

export type MethodFn = <
  TData = unknown,
  TError = unknown,
  Throw extends boolean = false,
  TStyle extends Style = 'fields',
>(
  options: { url: string; throwOnError?: Throw }
) => CallResult<TData, TError, Throw, TStyle>;

export type Client = { get: MethodFn };
export declare const client: Client;

export interface Options<Throw extends boolean = boolean> {
  client?: Client;
  throwOnError?: Throw;
}

export type Outcome<D, F extends boolean> = F extends true
  ? Promise<{ value: D extends object ? D[keyof D] : D; meta?: string }>
  : Promise<{ value: undefined; meta?: string }>;
export declare function outcome<D, F extends boolean = false>(): Outcome<D, F>;

export type N1<D, F extends boolean> = F extends true
  ? Promise<{ data: D; request: Request }>
  : Promise<{ data: D; request?: Request }>;
export declare function n1<D, F extends boolean = false>(): N1<D, F>;

export type N2<D, F extends boolean> = F extends true ? { data: D | undefined } : { data: undefined };
export declare function n2<D, F extends boolean = false>(): N2<D, F>;

export type N3<D, F extends boolean> = F extends true
  ? Promise<D & { request: Request }>
  : Promise<{ data: undefined }>;
export declare function n3<D, F extends boolean = false>(): N3<D, F>;

export type N6<D, F extends boolean> = F extends true
  ? Promise<{ data: D }>
  : Promise<{ data: undefined } & { data: D }>;
export declare function n6<D, F extends boolean = false>(): N6<D, F>;

export type N6b<D, F extends boolean> = F extends true
  ? Promise<{ data: D }>
  : Promise<{ data: undefined } & { data: D extends string ? never : unknown }>;
export declare function n6b<D, F extends boolean = false>(): N6b<D, F>;

export type Bare<D, F extends boolean> = F extends true
  ? Promise<{ value: D }>
  : Promise<{ value: undefined }>;
export declare function bare<D, F extends boolean = false>(): Bare<D, F>;

export type N7<D, F extends boolean> = F extends true
  ? Promise<{ data: D; then?: undefined }>
  : Promise<{ data: undefined }>;
export declare function n7<D, F extends boolean = false>(): N7<D, F>;

export type N8<D, F extends boolean> = F extends true
  ? Promise<{ data: D }>
  : Promise<{ data: undefined } & { data: string }>;
export declare function n8<D, F extends boolean = false>(): N8<D, F>;

export declare function viaDefault<D, F extends boolean = false, S = D>(): Outcome<S, F>;

export declare function over<D, F extends boolean = false>(a: string): Outcome<D, F>;
export declare function over<D, F extends boolean = false>(a: number): Outcome<D, F>;
`;

const SDK = `import { client } from './client';
import type { CallResult, Options, Outcome, Bare, N1, N2, N3, N6, N6b, N7, N8 } from './client';
import { outcome, bare, n1, n2, n3, n6, n6b, n7, n8, viaDefault, over } from './client';

export type ThingResponses = { 200: { id: string; n: number } };
export type ThingErrors = { 404: { message: string } };

export const getThing = <Throw extends boolean = false>(options: Options<Throw>): CallResult<ThingResponses, ThingErrors, Throw> =>
  (options.client ?? client).get<ThingResponses, ThingErrors, Throw>({ url: '/thing', ...options });

export const getThingData = <Throw extends boolean = false>(options: Options<Throw>): CallResult<ThingResponses, ThingErrors, Throw, 'data'> =>
  (options.client ?? client).get<ThingResponses, ThingErrors, Throw, 'data'>({ url: '/thing-data', ...options });

export const p1b = <F extends boolean = false>(): Outcome<ThingResponses, F> => outcome<ThingResponses, F>();

export const wn1 = <F extends boolean = false>(): N1<ThingResponses, F> => n1<ThingResponses, F>();
export const wn2 = <F extends boolean = false>(): N2<ThingResponses, F> => n2<ThingResponses, F>();
export const wn3 = <F extends boolean = false>(): N3<ThingResponses, F> => n3<ThingResponses, F>();
export const wn6 = <F extends boolean = false>(): N6<ThingResponses, F> => n6<ThingResponses, F>();
export const wn6b = <F extends boolean = false>(): N6b<ThingResponses, F> => n6b<ThingResponses, F>();
export const wp3 = <F extends boolean = false>(): Bare<ThingResponses, F> => bare<ThingResponses, F>();
export const wn7 = <F extends boolean = false>(): N7<ThingResponses, F> => n7<ThingResponses, F>();
export const wn8 = <F extends boolean = false>(): N8<ThingResponses, F> => n8<ThingResponses, F>();
export const wn9 = <F extends boolean = false>(): Outcome<ThingResponses, F> => viaDefault<ThingResponses, F>();
export const wn10 = <F extends boolean = false>(): Outcome<ThingResponses, F> => over<ThingResponses, F>('x');
`;

const CALLS = {
  p1: "(options.client ?? client).get<ThingResponses, ThingErrors, Throw>({ url: '/thing', ...options })",
  p1b: 'outcome<ThingResponses, F>()',
  p2: "(options.client ?? client).get<ThingResponses, ThingErrors, Throw, 'data'>({ url: '/thing-data', ...options })",
  n1: 'n1<ThingResponses, F>()',
  n2: 'n2<ThingResponses, F>()',
  n3: 'n3<ThingResponses, F>()',
  n6: 'n6<ThingResponses, F>()',
  n6b: 'n6b<ThingResponses, F>()',
  p3: 'bare<ThingResponses, F>()',
  n7: 'n7<ThingResponses, F>()',
  n8: 'n8<ThingResponses, F>()',
  n9: 'viaDefault<ThingResponses, F>()',
  n10: "over<ThingResponses, F>('x')",
} as const;
type Name = keyof typeof CALLS;
const NAMES = Object.keys(CALLS) as Name[];

/** What the call's result prints as before any rewrite. */
const OWN_RESULT = {
  p1: 'Awaited<CallResult<ThingResponses, ThingErrors, Throw, "fields">>',
  p1b: 'Awaited<Outcome<ThingResponses, F>>',
} as const;

const reasonFor = (quoted: string) =>
  `the call's type argument does not reach its result ('${quoted}'), so it does not state the response`;

const lineOf = (text: string): number =>
  SDK.split('\n')
    .map((line, index) => (line.includes(text) ? index + 1 : 0))
    .filter(Boolean)[0];

interface Outcome1 {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'abstain' | 'wider';
  reason?: string;
}

interface Answer {
  reason?: string;
  fresh_type_reason?: string;
  undefined_free_reason?: string;
}

class Repo {
  client = new SidecarClient();
  dir = '';
  sdkPath = '';

  constructor(private readonly strict: boolean) {}

  async start(): Promise<void> {
    this.dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3165-'));
    fs.mkdirSync(path.join(this.dir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(this.dir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: this.strict,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(this.dir, 'src', 'client.ts'), CLIENT);
    this.sdkPath = path.join(this.dir, 'src', 'sdk.ts');
    fs.writeFileSync(this.sdkPath, SDK);
    await this.client.start();
    await this.client.send({ action: 'init', request_id: 'init', repo_root: this.dir });
  }

  async stop(): Promise<void> {
    await this.client.stop();
    fs.rmSync(this.dir, { recursive: true, force: true });
  }

  locator(name: Name) {
    const line = lineOf(CALLS[name]);
    return {
      file_path: this.sdkPath,
      line_number: line,
      expression_text: CALLS[name],
      expression_line: line,
    };
  }

  async prescreen(names: Name[]): Promise<Map<string, Answer>> {
    const res = await this.client.send<{
      status: string;
      prescreens?: Array<Answer & { item_id: string }>;
    }>({
      action: 'retype_prescreen',
      request_id: 'prescreen',
      items: names.map((name) => ({ item_id: name, ...this.locator(name) })),
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    return new Map(res.prescreens!.map((p) => [p.item_id, p]));
  }

  async retype(name: Name, producerType: string): Promise<Outcome1> {
    const res = await this.client.send<{ status: string; outcomes?: Outcome1[] }>({
      action: 'retype_check',
      request_id: `${name}-${producerType}`,
      items: [{ item_id: name, ...this.locator(name), producer_type: producerType, wire: true }],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    return res.outcomes![0];
  }
}

describe('carrick#3165: the retype decides a deferred result before the rewrite', () => {
  const strict = new Repo(true);
  const loose = new Repo(false);

  before(async () => {
    await strict.start();
    await loose.start();
  });

  after(async () => {
    await strict.stop();
    await loose.stop();
  });

  it('leaves every case to the rewrite as far as `reason` and tier 1 go', async () => {
    const answers = await strict.prescreen(NAMES);
    for (const name of NAMES) {
      assert.strictEqual(answers.get(name)?.reason, undefined, name);
      assert.strictEqual(answers.get(name)?.fresh_type_reason, undefined, name);
    }
  });

  it('answers undefined_free_reason for the two deferred-result shapes and no other', async () => {
    const answers = await strict.prescreen(NAMES);
    const decided = NAMES.filter((name) => answers.get(name)?.undefined_free_reason !== undefined);
    // p1: the generated client's call. p1b, p3: other deferred results whose
    // every member is typed with undefined by an arm that never names the
    // stated type.
    assert.deepStrictEqual(decided, ['p1', 'p1b', 'p3']);
    // p2: the 'data' style puts the stated type at a structural position.
    // n1: no arm types `data` with undefined. n2: the result is not Awaited
    // of a conditional. n3: a type parameter at a structural position.
    // n6, n6b, n8: a member declared twice in an intersection. n7: a `then`
    // member. n9: a default that names a type parameter. n10: two signatures.
  });

  it('answers nothing in a program without strictNullChecks', async () => {
    // p3 types every member `undefined` in an arm, which the loose program
    // erases from the unions the reach test reads.
    const answers = await loose.prescreen(['p1', 'p1b', 'p3']);
    for (const name of ['p1', 'p1b', 'p3']) {
      assert.strictEqual(answers.get(name)?.undefined_free_reason, undefined, name);
    }
  });

  it("quotes the call's own result before any rewrite, word for word", async () => {
    const answers = await strict.prescreen(['p1', 'p1b']);
    assert.strictEqual(answers.get('p1')!.undefined_free_reason, reasonFor(OWN_RESULT.p1));
    assert.strictEqual(answers.get('p1b')!.undefined_free_reason, reasonFor(OWN_RESULT.p1b));
  });

  // The proof's check: for every producer text the scanner's gate accepts,
  // the full retype abstains with the same sentence (its quoted type is the
  // one after the rewrite, so it is matched by shape).
  const UNDEFINED_FREE = [
    '{ id: string; n: number }',
    '{ id: string; n: number }[]',
    'never[]',
    '{ a: string } | null',
    '{ a: string } | { b: number }',
    '{}',
    'string',
    'true',
    '"x" | "y"',
    '[string, number]',
    '{ a: 1 } & { b: 2 }',
    '{ at: Date }',
    '{ (): void; a: string }',
    'never',
  ];
  const SHAPE = /^the call's type argument does not reach its result \('.*'\), so it does not state the response$/;
  for (const name of ['p1', 'p1b'] as const) {
    const producers = name === 'p1' ? [...UNDEFINED_FREE, '{ a: Throw }'] : UNDEFINED_FREE;
    for (const producer of producers) {
      it(`(${name}) the full retype abstains for ${producer}`, async () => {
        const reason = (await strict.prescreen([name])).get(name)!.undefined_free_reason;
        assert.ok(reason, name);
        const out = await strict.retype(name, producer);
        assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
        assert.match(out.reason!, SHAPE);
      });
    }
  }

  // The boundary each guard keeps: these are judged, not abstained.
  for (const [name, producer] of [
    ['p1', 'string | undefined'],
    ['p1b', 'string | undefined'],
    ['p1', 'Response | undefined'],
    ['p1', '{ toJSON(): undefined }'],
  ] as const) {
    it(`(${name}) a producer with a top-level undefined reaches a slot: ${producer}`, async () => {
      const out = await strict.retype(name, producer);
      assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    });
  }

  for (const producer of ['{ id: string; n: number }[]', 'never[]']) {
    it(`(p1) a program without strictNullChecks reaches a slot for ${producer}`, async () => {
      const out = await loose.retype('p1', producer);
      assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    });
  }

  it('(p3) a program without strictNullChecks reaches a slot for never[]', async () => {
    const out = await loose.retype('p3', 'never[]');
    assert.notStrictEqual(out.outcome, 'abstain', JSON.stringify(out));
  });

  // n6b: with the call's own argument the second declaration of `data` is
  // `unknown`, so the arm still types it `undefined`; with a string stated it
  // is `never`, the arm vanishes and the stated type reaches `data`.
  it('(n6b) a string stated at an intersection that reduces to never reaches a slot', async () => {
    const out = await strict.retype('n6b', 'string');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
  });

  for (const name of ['n1', 'n2', 'n6', 'n8'] as const) {
    it(`(${name}) the full retype reaches a slot, so the prescreen must not decide it`, async () => {
      const out = await strict.retype(name, '{ id: string; n: number }');
      assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    });
  }
});
