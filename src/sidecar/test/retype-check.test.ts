/**
 * carrick#1491: an untyped consumer call is judged by retyping it with the
 * producer's response type and reading what the consumer file's own
 * type-check says.
 *
 * The client declared below is hand-written and synthetic: a generic
 * instance whose `post<T>` resolves to an envelope carrying `T` as `data`
 * beside a request-data parameter that defaults to `any`. That default is
 * what leaves both call shapes without a comparable type on a real scan:
 * the untyped call's payload is `any`, and the typed call's envelope carries
 * an `any` beside its payload.
 *
 * The two cases the ticket requires, (a) a typed call and (b) the same call
 * with no type argument, both read `response.data.x`. Against a producer
 * returning `{ y }` both must be flagged at the read; against one returning
 * `{ x }` neither may be. The rest pin the edges: a typed wrapper whose
 * declared return is the sink, a call whose result goes nowhere, an untyped
 * helper retyped by a cast, and a producer type the consumer cannot resolve.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const CLIENT_DECL = `declare module 'http-client' {
  export interface RequestOptions<D = any> {
    baseURL?: string;
    data?: D;
  }
  export interface ClientResponse<T = any, D = any> {
    data: T;
    status: number;
    options: RequestOptions<D>;
  }
  export interface ClientInstance {
    post<T = any, R = ClientResponse<T>, D = any>(url: string, data?: D): Promise<R>;
  }
  export function create(options?: RequestOptions): ClientInstance;
}
`;

// Line numbers are read off this text; keep the two in step.
const CONSUMER = `import { create } from 'http-client';

const api = create({ baseURL: 'http://localhost:3000' });

export async function typedRead(): Promise<number> {
  const response = await api.post<{ x: number }>('/p');
  return response.data.x;
}

export async function untypedRead(): Promise<number> {
  const response = await api.post('/p');
  return response.data.x;
}

export interface Summary {
  x: number;
}

export async function typedWrapper(): Promise<Summary> {
  const response = await api.post('/p');
  return response.data;
}

export async function discarded(): Promise<void> {
  await api.post('/p');
}

async function request(path: string): Promise<any> {
  return fetch(path).then((r) => r.json());
}

export async function untypedHelper(): Promise<number> {
  const body = await request('/p');
  return body.x;
}

export async function alreadyBroken(): Promise<number> {
  const response = await api.post('/p');
  const n: string = 1;
  return response.data.x + n.length;
}

declare function submit<D>(url: string, data: D): Promise<{ ok: boolean }>;

export async function bodyGeneric(): Promise<boolean> {
  const sent = await submit('/p', { a: 1 });
  return sent.ok;
}

// A client the program cannot resolve, as on a checkout without its
// dependencies: everything it returns is \`any\`.
import { create as createMissing } from 'not-installed-client';
const missing = createMissing();

export async function unresolvedTyped(): Promise<number> {
  const response = await missing.post<{ x: number }>('/p');
  return response.data.x;
}

export async function unresolvedUntyped(): Promise<number> {
  const response = await missing.post('/p');
  return response.data.x;
}

declare function getBare<T>(url: string): Promise<{ data: T; status: number }>;

export async function bareEnvelope(): Promise<number> {
  const response = await getBare('/p');
  return response.data.x;
}

interface Stream<T> {
  subscribe(listener: (value: T) => void): void;
}
declare function watch<T>(url: string): Promise<Stream<T>>;

export async function streamed(): Promise<void> {
  const stream = await watch('/p');
  stream.subscribe((value) => console.log(value.x));
}
`;

const CASES = {
  typed: { line: 6, text: "api.post<{ x: number }>('/p')", read: 7 },
  untyped: { line: 11, text: "api.post('/p')", read: 12 },
  wrapper: { line: 20, text: "api.post('/p')", read: 21 },
  discarded: { line: 25, text: "api.post('/p')" },
  helper: { line: 33, text: "request('/p')", read: 34 },
  broken: { line: 38, text: "api.post('/p')", read: 40 },
  bodyGeneric: { line: 46, text: "submit('/p', { a: 1 })" },
  unresolvedTyped: { line: 56, text: "missing.post<{ x: number }>('/p')" },
  unresolvedUntyped: { line: 61, text: "missing.post('/p')" },
  bareEnvelope: { line: 68, text: "getBare('/p')", read: 69 },
  streamed: { line: 78, text: "watch('/p')", read: 79 },
} as const;

interface Outcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'abstain';
  form?: string;
  diagnostics: Array<{ line: number; code: number; message: string }>;
  reason?: string;
}

describe('carrick#1491: retype an untyped consumer call with the producer response', () => {
  let client: SidecarClient;
  let repoDir: string;
  let consumerPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1491-'));
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
    fs.writeFileSync(path.join(repoDir, 'src', 'http-client.d.ts'), CLIENT_DECL);
    consumerPath = path.join(repoDir, 'src', 'client.ts');
    fs.writeFileSync(consumerPath, CONSUMER);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function retype(
    name: keyof typeof CASES,
    producerType: string,
    wire = true
  ): Promise<Outcome> {
    const c = CASES[name];
    const res = await client.send<{ status: string; outcomes?: Outcome[]; errors?: string[] }>({
      action: 'retype_check',
      request_id: `${name}-${producerType}`,
      items: [
        {
          item_id: name,
          file_path: consumerPath,
          line_number: c.line,
          expression_text: c.text,
          expression_line: c.line,
          producer_type: producerType,
          wire,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    assert.strictEqual(res.outcomes?.length, 1);
    return res.outcomes![0];
  }

  for (const name of ['typed', 'untyped'] as const) {
    it(`(${name}) flags the read of a field the producer does not return`, async () => {
      const out = await retype(name, '{ y: number; }');
      assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
      assert.strictEqual(out.form, 'type_argument');
      assert.strictEqual(out.diagnostics.length, 1, JSON.stringify(out.diagnostics));
      const [d] = out.diagnostics;
      assert.strictEqual(d.line, CASES[name].read);
      assert.strictEqual(d.code, 2339);
      assert.match(d.message, /Property 'x' does not exist on type '\{ y: number; \}'/);
    });

    it(`(${name}) flags nothing when the producer returns the field`, async () => {
      const out = await retype(name, '{ x: number; }');
      assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
      assert.deepStrictEqual(out.diagnostics, []);
    });
  }

  it('reads a typed wrapper: the declared return is where the response lands', async () => {
    const out = await retype('wrapper', '{ y: number; }');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.strictEqual(out.diagnostics[0].line, CASES.wrapper.read);
    assert.strictEqual(out.diagnostics[0].code, 2741);

    const agreeing = await retype('wrapper', '{ x: number; extra: string; }');
    assert.strictEqual(agreeing.outcome, 'agrees', JSON.stringify(agreeing));
  });

  it('abstains when the type parameter it would fill does not carry the response', async () => {
    const out = await retype('bodyGeneric', '{ ok: boolean; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /does not reach its result/);
  });

  it('abstains when the client does not resolve, typed or not', async () => {
    // Its result may be an envelope around the payload, so neither stating a
    // type argument nor casting to the payload would say anything true.
    const typed = await retype('unresolvedTyped', '{ y: number; }');
    assert.strictEqual(typed.outcome, 'abstain', JSON.stringify(typed));
    assert.match(typed.reason ?? '', /does not reach its result/);
    const untyped = await retype('unresolvedUntyped', '{ y: number; }');
    assert.strictEqual(untyped.outcome, 'abstain', JSON.stringify(untyped));
    assert.match(untyped.reason ?? '', /does not resolve in its program/);
  });

  it('follows the stated type into the result, as a member or a type argument', async () => {
    // An anonymous envelope carries the type only as a member.
    const bare = await retype('bareEnvelope', '{ y: number; }');
    assert.strictEqual(bare.outcome, 'mismatch', JSON.stringify(bare));
    assert.strictEqual(bare.diagnostics[0].line, CASES.bareEnvelope.read);
    // A stream carries it only as a type argument.
    const streamed = await retype('streamed', '{ y: number; }');
    assert.strictEqual(streamed.outcome, 'mismatch', JSON.stringify(streamed));
    assert.strictEqual(streamed.diagnostics[0].line, CASES.streamed.read);
  });

  it('abstains when the producer type carries a comment', async () => {
    // Collapsed onto one line, a line comment would swallow the rest of the call.
    const out = await retype('untyped', '{ y: number; // the total\n }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /carries a comment/);
  });

  it('names the producer type as declared, not as the wire transform', async () => {
    const out = await retype('untyped', '{ when: Date; }');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.match(out.diagnostics[0].message, /on type '\{ when: Date; \}'/);
  });

  it('abstains when nothing reads the response', async () => {
    const out = await retype('discarded', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain');
    assert.match(out.reason ?? '', /never reads the response/);
  });

  it('retypes an untyped helper with a cast of its any result', async () => {
    const out = await retype('helper', '{ y: number; }');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.strictEqual(out.form, 'cast');
    assert.strictEqual(out.diagnostics[0].line, CASES.helper.read);
    assert.strictEqual(out.diagnostics[0].code, 2339);
  });

  it('does not count a diagnostic the file already had', async () => {
    const agreeing = await retype('broken', '{ x: number; }');
    assert.strictEqual(agreeing.outcome, 'agrees', JSON.stringify(agreeing));

    const out = await retype('broken', '{ y: number; }');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.deepStrictEqual(
      out.diagnostics.map((d) => [d.line, d.code]),
      [[CASES.broken.read, 2339]]
    );
  });

  it('compares the form JSON puts on the wire', async () => {
    // `untypedRead` returns `response.data.x` where it declares a number. A
    // value whose toJSON() returns a number arrives as one; the declared
    // form is not one, and a Date is neither.
    const wireAgrees = await retype('untyped', '{ x: { toJSON(): number }; }');
    assert.strictEqual(wireAgrees.outcome, 'agrees', JSON.stringify(wireAgrees));
    const declared = await retype('untyped', '{ x: { toJSON(): number }; }', false);
    assert.strictEqual(declared.outcome, 'mismatch', JSON.stringify(declared));
    const date = await retype('untyped', '{ x: Date; }');
    assert.strictEqual(date.outcome, 'mismatch', JSON.stringify(date));
    assert.strictEqual(date.diagnostics[0].code, 2322);
  });

  it('abstains when the producer type names something the consumer cannot see', async () => {
    const out = await retype('untyped', '{ x: ProducerOnlyName; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /does not resolve in the consumer's program: TS2304/);
  });

  it('abstains on what its budget does not reach', async () => {
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
      action: 'retype_check',
      request_id: 'budget',
      budget_ms: 0,
      items: [
        {
          item_id: 'untyped',
          file_path: consumerPath,
          line_number: CASES.untyped.line,
          expression_text: CASES.untyped.text,
          expression_line: CASES.untyped.line,
          producer_type: '{ y: number; }',
          wire: true,
        },
      ],
    });
    assert.strictEqual(res.status, 'success');
    assert.strictEqual(res.outcomes?.[0]?.outcome, 'abstain');
    assert.match(res.outcomes?.[0]?.reason ?? '', /ran out of its 0ms budget/);
  });

  it('leaves the program as it found it', async () => {
    await retype('untyped', '{ y: number; }');
    const res = await client.send<{ inferred_types?: Array<{ type_string: string }> }>({
      action: 'infer',
      request_id: 'after',
      requests: [
        {
          file_path: consumerPath,
          line_number: CASES.typed.line,
          infer_kind: 'call_result',
          alias: 'after',
          expression_text: CASES.typed.text,
          expression_line: CASES.typed.line,
        },
      ],
    });
    assert.match(res.inferred_types?.[0]?.type_string ?? '', /\{ x: number; \}/);
  });
});
