/**
 * carrick#1493: a consumer that calls `fetch` and reads the body later is
 * judged by retyping the BODY READ with the producer's response type.
 *
 * `fetch` takes no type argument and returns a typed `Response`, so the call
 * the row locates cannot carry the producer's type. The payload is
 * `res.json()` on the binding of that call's result: the retype casts that
 * read to the producer's type (or replaces a cast the source wrote around
 * it) and diffs the file's diagnostics as it does for a typed call.
 *
 * The guards: every body read must be on THIS call's result, the response
 * object must not be handed anywhere its body could be read out of view, the
 * body read must not already state a type, and the body itself must not
 * escape the file's own type-check.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

// Line numbers are read off this text; keep the two in step.
const CONSUMER = `export interface Order {
  x: number;
}

export async function bodyRead(): Promise<number> {
  const res = await fetch('/p');
  const body = await res.json();
  return body.x;
}

export async function chained(): Promise<number> {
  const body = await (await fetch('/p')).json();
  return body.x;
}

export async function statusFirst(): Promise<number> {
  const res = await fetch('/p');
  if (!res.ok) throw new Error(String(res.status));
  const body = await res.json();
  return body.x;
}

export async function castBody(): Promise<number> {
  const res = await fetch('/p');
  const body = (await res.json()) as Order;
  return body.x;
}

export async function annotatedBody(): Promise<void> {
  const res = await fetch('/p');
  const body: Order = await res.json();
  console.log(body);
}

export async function returnedBody() {
  const res = await fetch('/p');
  const body = (await res.json()) as Order;
  return body;
}

declare function log(res: Response): void;

export async function handedOn(): Promise<number> {
  const res = await fetch('/p');
  log(res);
  const body = await res.json();
  return body.x;
}

export async function statusOnly(): Promise<number> {
  const res = await fetch('/p');
  return res.status;
}

export async function discardedBody(): Promise<boolean> {
  const res = await fetch('/p');
  await res.json();
  return res.ok;
}

export async function reassigned(retry: boolean): Promise<number> {
  let res = await fetch('/p');
  if (retry) res = await fetch('/q');
  const body = await res.json();
  return body.x;
}

declare function typedGet(url: string): Promise<{ json(): Promise<{ x: number }> }>;

export async function typedBody(): Promise<number> {
  const res = await typedGet('/p');
  const body = await res.json();
  return body.x;
}

export async function inCallback(): Promise<number> {
  const res = await fetch('/p');
  return res.json().then((body) => body.x);
}

export async function castPromise(): Promise<number> {
  const res = await fetch('/p');
  const body = await (res.json() as Promise<Order>);
  return body.x;
}

export async function arrowBody(): Promise<Order> {
  const res = await fetch('/p');
  const read = (): Promise<Order> => res.json();
  return read();
}

declare function lookup(url: string): Promise<{ json(key: string): any }>;

export async function keyedJson(): Promise<number> {
  const res = await lookup('/p');
  const value = await res.json('k');
  return value.x;
}

declare function settle(body: Promise<Order>): Promise<number>;

export async function passedRead(): Promise<number> {
  const res = await fetch('/p');
  return settle(res.json());
}

declare function parseBody(value: unknown): any;

export async function errorBranch(): Promise<number> {
  const res = await fetch('/p');
  if (!res.ok) {
    const e = await res.json();
    throw new Error(e.message);
  }
  const body = await res.json();
  return body.x;
}

export async function errorOnly(): Promise<number> {
  const res = await fetch('/p');
  if (!res.ok) {
    const e = await res.json();
    throw new Error(e.message);
  }
  return res.status;
}

export async function sharedRead(): Promise<number> {
  const res = await fetch('/p');
  const data = await res.json();
  if (!res.ok) throw new Error(data.message);
  return data.x;
}

export async function okElse(): Promise<number> {
  const res = await fetch('/p');
  if (res.ok) {
    const body = await res.json();
    return body.x;
  } else {
    const e = await res.json();
    throw new Error(e.message);
  }
}

export async function okFirst(): Promise<number> {
  const res = await fetch('/p');
  if (res.ok) return (await res.json()).x;
  const e = await res.json();
  throw new Error(e.message);
}

export async function statusGuard(): Promise<number> {
  const res = await fetch('/p');
  if (res.status >= 400) {
    const e = await res.json();
    throw new Error(e.error);
  }
  const body = await res.json();
  return body.x;
}

export async function statusOdd(): Promise<number> {
  const res = await fetch('/p');
  if (res.status > 100) {
    const e = await res.json();
    return e.x;
  }
  return 0;
}

export async function doubleCast(): Promise<number> {
  const res = await fetch('/p');
  const body = (await res.json()) as unknown as Order;
  return body.x;
}

export async function angleCast(): Promise<number> {
  const res = await fetch('/p');
  const body = <Order>(await res.json());
  return body.x;
}

export async function parsedBody(): Promise<number> {
  const res = await fetch('/p');
  const body = parseBody(await res.json());
  return body.x;
}

declare class Wrapped {
  constructor(value: unknown);
  x: number;
}

export async function constructed(): Promise<number> {
  const res = await fetch('/p');
  const body = new Wrapped(await res.json());
  return body.x;
}

export async function errorFirst(): Promise<number> {
  const res = await fetch('/p');
  const payload = await res.json();
  if (!res.ok) throw new Error(payload.message);
  return res.status;
}

export async function notFound(): Promise<number> {
  const res = await fetch('/p');
  if (res.status === 404) return 0;
  const body = await res.json();
  return body.x;
}

export async function statusNot200(): Promise<number> {
  const res = await fetch('/p');
  if (res.status !== 200) throw new Error((await res.json()).error);
  const body = await res.json();
  return body.x;
}

export async function ternary(): Promise<number> {
  const res = await fetch('/p');
  return res.ok ? (await res.json()).x : (await res.json()).code;
}

export async function switchStatus(): Promise<number> {
  const res = await fetch('/p');
  switch (res.status) {
    case 200:
      return (await res.json()).x;
    default:
      return 0;
  }
}

export async function compoundTest(retry: boolean): Promise<number> {
  const res = await fetch('/p');
  if (res.ok || retry) {
    const body = await res.json();
    return body.x;
  }
  return 0;
}

export async function okBlockFirst(): Promise<number> {
  const res = await fetch('/p');
  if (res.ok) {
    const body = await res.json();
    return body.x;
  }
  const e = await res.json();
  throw new Error(e.message);
}

export async function destructured(): Promise<number> {
  const res = await fetch('/p');
  const { x, message } = await res.json();
  if (!res.ok) throw new Error(message);
  return x;
}
`;

const CASES = {
  bodyRead: { line: 6, text: "fetch('/p')", read: 8 },
  chained: { line: 12, text: "fetch('/p')", read: 13 },
  statusFirst: { line: 17, text: "fetch('/p')", read: 20 },
  castBody: { line: 24, text: "fetch('/p')", read: 26 },
  annotatedBody: { line: 30, text: "fetch('/p')", read: 31 },
  returnedBody: { line: 36, text: "fetch('/p')" },
  handedOn: { line: 44, text: "fetch('/p')" },
  statusOnly: { line: 51, text: "fetch('/p')" },
  discardedBody: { line: 56, text: "fetch('/p')" },
  reassigned: { line: 62, text: "fetch('/p')" },
  typedBody: { line: 71, text: "typedGet('/p')" },
  inCallback: { line: 77, text: "fetch('/p')", read: 78 },
  castPromise: { line: 82, text: "fetch('/p')", read: 84 },
  arrowBody: { line: 88, text: "fetch('/p')", read: 89 },
  keyedJson: { line: 96, text: "lookup('/p')" },
  passedRead: { line: 104, text: "fetch('/p')", read: 105 },
  locatedAtRead: { line: 7, text: 'res.json()', read: 8 },
  errorBranch: { line: 111, text: "fetch('/p')", read: 117 },
  errorOnly: { line: 121, text: "fetch('/p')" },
  sharedRead: { line: 130, text: "fetch('/p')" },
  okElse: { line: 137, text: "fetch('/p')", read: 140 },
  okFirst: { line: 148, text: "fetch('/p')", read: 149 },
  statusGuard: { line: 155, text: "fetch('/p')", read: 161 },
  statusOdd: { line: 165, text: "fetch('/p')" },
  doubleCast: { line: 174, text: "fetch('/p')" },
  angleCast: { line: 180, text: "fetch('/p')" },
  parsedBody: { line: 186, text: "fetch('/p')" },
  constructed: { line: 197, text: "fetch('/p')" },
  errorFirst: { line: 203, text: "fetch('/p')" },
  notFound: { line: 210, text: "fetch('/p')", read: 213 },
  statusNot200: { line: 217, text: "fetch('/p')", read: 220 },
  ternary: { line: 224, text: "fetch('/p')", read: 225 },
  switchStatus: { line: 229, text: "fetch('/p')" },
  okBlockFirst: { line: 248, text: "fetch('/p')", read: 251 },
  destructured: { line: 258, text: "fetch('/p')" },
  compoundTest: { line: 239, text: "fetch('/p')" },
} as const;

interface Outcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'abstain';
  diagnostics: Array<{ line: number; code: number; message: string }>;
  reason?: string;
}

describe('carrick#1493: retype a body read off a fetch Response', () => {
  let client: SidecarClient;
  let repoDir: string;
  let consumerPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1493-'));
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
    consumerPath = path.join(repoDir, 'src', 'orders.ts');
    fs.writeFileSync(consumerPath, CONSUMER);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function retype(name: keyof typeof CASES, producerType: string): Promise<Outcome> {
    const c = CASES[name];
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
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
          wire: true,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    return res.outcomes![0];
  }

  for (const name of [
    'bodyRead',
    'chained',
    'statusFirst',
    'castBody',
    'castPromise',
    'inCallback',
    'locatedAtRead',
    'errorBranch',
    'okElse',
    'okFirst',
    'statusGuard',
    'notFound',
    'statusNot200',
    'ternary',
    'okBlockFirst',
  ] as const) {
    it(`(${name}) flags the read of a field the producer does not return`, async () => {
      const out = await retype(name, '{ y: number; }');
      assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
      assert.strictEqual(out.diagnostics.length, 1, JSON.stringify(out.diagnostics));
      assert.strictEqual(out.diagnostics[0].line, CASES[name].read);
      assert.strictEqual(out.diagnostics[0].code, 2339);
      assert.match(out.diagnostics[0].message, /Property 'x' does not exist on type '\{ y: number; \}'/);
    });

    it(`(${name}) flags nothing when the producer returns the field`, async () => {
      const out = await retype(name, '{ x: number; }');
      assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
    });
  }

  it('reads an annotated body: the declared type is where the body lands', async () => {
    const out = await retype('annotatedBody', '{ y: number; }');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.strictEqual(out.diagnostics[0].line, CASES.annotatedBody.read);
    assert.strictEqual(out.diagnostics[0].code, 2741);
    const agreeing = await retype('annotatedBody', '{ x: number; extra: string; }');
    assert.strictEqual(agreeing.outcome, 'agrees', JSON.stringify(agreeing));
  });

  for (const name of ['arrowBody'] as const) {
    it(`(${name}) judges a body read handed straight to a typed place`, async () => {
      // The cast read is parenthesised; the finding must still land on the
      // read's own line and not read as the producer's type failing.
      const out = await retype(name, '{ y: number; }');
      assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
      assert.strictEqual(out.diagnostics[0].line, CASES[name].read);
      const agreeing = await retype(name, '{ x: number; }');
      assert.strictEqual(agreeing.outcome, 'agrees', JSON.stringify(agreeing));
    });
  }

  it('does not take a json() call with arguments for a body read', async () => {
    const out = await retype('keyedJson', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /takes no type argument/);
  });

  it('compares the form JSON puts on the wire', async () => {
    const out = await retype('bodyRead', '{ x: Date; }');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.strictEqual(out.diagnostics[0].code, 2322);
  });

  it('abstains when the producer type names something the consumer cannot see', async () => {
    const out = await retype('bodyRead', '{ x: ProducerOnlyName; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /does not resolve in the consumer's program: TS2304/);
  });

  it('abstains when the body is returned from a function with no declared return type', async () => {
    const out = await retype('returnedBody', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /readers are elsewhere/);
  });

  it('abstains when the response object is handed on', async () => {
    const out = await retype('handedOn', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /response object is handed on/);
  });

  it('abstains when no JSON body is read', async () => {
    const out = await retype('statusOnly', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /takes no type argument/);
  });

  it('abstains when the body read goes nowhere', async () => {
    const out = await retype('discardedBody', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /never reads the response body/);
  });

  it('abstains when the binding may hold another response', async () => {
    const out = await retype('reassigned', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /reassigned/);
  });

  it('retypes only the success path: a read on the error path alone abstains', async () => {
    const out = await retype('errorOnly', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /only on its error path/);
  });

  it('retypes only the success path: a read used only on the error path abstains', async () => {
    const out = await retype('errorFirst', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /only on its error path/);
  });

  for (const name of ['sharedRead', 'destructured'] as const) {
    it(`(${name}) abstains when one body read serves both the success and the error path`, async () => {
      const out = await retype(name, '{ x: number; }');
      assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
      assert.match(out.reason ?? '', /both the success and the error path/);
    });
  }

  for (const name of ['statusOdd', 'switchStatus', 'compoundTest'] as const) {
    it(`(${name}) abstains when a status test does not say which side fails`, async () => {
      const out = await retype(name, '{ x: number; }');
      assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
      assert.match(out.reason ?? '', /failing side is unclear/);
    });
  }

  for (const name of ['doubleCast', 'angleCast', 'parsedBody', 'passedRead', 'constructed'] as const) {
    it(`(${name}) abstains when a cast, assertion or call around the read types it`, async () => {
      // Replacing only the innermost type would let the outer one agree with
      // any producer.
      for (const producer of ['{ y: number; }', '{ x: number; }']) {
        const out = await retype(name, producer);
        assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
        assert.match(out.reason ?? '', /already states a type/);
      }
    });
  }

  it('abstains when the body read already states a type', async () => {
    const out = await retype('typedBody', '{ y: number; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /already states a type/);
  });

  it('leaves the file as it found it', async () => {
    await retype('bodyRead', '{ y: number; }');
    await retype('castBody', '{ y: number; }');
    const again = await retype('bodyRead', '{ x: number; }');
    assert.strictEqual(again.outcome, 'agrees', JSON.stringify(again));
    const res = await client.send<{ inferred_types?: Array<{ type_string: string }> }>({
      action: 'infer',
      request_id: 'after',
      requests: [
        {
          file_path: consumerPath,
          line_number: CASES.castBody.line,
          infer_kind: 'call_result',
          alias: 'after',
          expression_text: CASES.castBody.text,
          expression_line: CASES.castBody.line,
        },
      ],
    });
    assert.match(res.inferred_types?.[0]?.type_string ?? '', /Order|x: number/);
  });
});
