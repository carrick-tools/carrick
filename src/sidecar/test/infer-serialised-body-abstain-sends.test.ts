/**
 * carrick#5133: a serialised-body envelope handler with a branch that cannot
 * be read still answers `unknown` with `serialised_body_unread` (carrick#2200),
 * and that answer names where the handler sends the values it found
 * (`read_spans`): each read branch's payload, and a payload whose type holds
 * `any` or `unknown` somewhere inside it. A branch with no value found, such
 * as a ready-made reply handed back from another result, names nothing, and a
 * handler with no value found anywhere names nothing at all.
 *
 * The edit-time check places a row by these positions and compares each sent
 * value with what a caller expects; the stated type is untouched.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ORDERS_TS = `export interface Reply {
  statusCode: number;
  body: string;
}

function reply(code: number, payload: unknown): Reply {
  return { statusCode: code, body: JSON.stringify(payload) };
}

type Lookup = { id: string } | { error: Reply };
declare function lookup(key: string): Lookup;
declare const readyMade: Reply;
declare const extras: { [x: string]: unknown };

export interface OrderSummary {
  id: string;
  lines: number;
}

export async function orderSummary(key: string): Promise<Reply> { // GUARDED
  const found = lookup(key);
  if ("error" in found) return found.error;
  const summary: OrderSummary = { id: found.id, lines: 1 };
  return reply(200, summary);
}

export async function orderWithExtras(key: string): Promise<Reply> { // HOLE
  const found = lookup(key);
  if ("error" in found) return found.error;
  return reply(200, { id: found.id, extras });
}

export async function orderPassedOn(key: string): Promise<Reply> { // NONE_READ
  const found = lookup(key);
  if ("error" in found) return found.error;
  if (!found.id) return reply(404, { missing: key });
  return readyMade;
}

export async function orderRaw(key: string, raw: unknown): Promise<Reply> { // BARE_UNKNOWN
  const found = lookup(key);
  if ("error" in found) return found.error;
  return reply(200, raw);
}
`;

const PREBUILT =
  'A return hands back a status and body built somewhere else, such as a variable or a field of another result, so the value in its body was not read.';
const TOP_TYPE = 'A return serialises a value typed any or unknown, so the shape of its body is not known.';

interface Provenance {
  path: string;
  kind: string;
  reason: string;
  detail?: string;
}

interface ReadSpan {
  file_path: string;
  start: number;
  end: number;
}

interface Inferred {
  alias: string;
  type_string: string;
  is_explicit: boolean;
  any_provenance?: Provenance[];
  read_spans?: ReadSpan[];
}

interface InferShape {
  inferred_types?: Inferred[];
}

function lineOf(marker: string): number {
  const idx = ORDERS_TS.split('\n').findIndex((l) => l.includes(`// ${marker}`));
  assert.ok(idx >= 0, `fixture must contain: // ${marker}`);
  return idx + 1;
}

describe('carrick#5133: an unread serialised-body handler names the sends it found', () => {
  let repoDir: string;
  let client: SidecarClient;
  let ordersPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-5133-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          rootDir: 'src',
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    ordersPath = path.join(repoDir, 'src', 'orders.ts');
    fs.writeFileSync(ordersPath, ORDERS_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function inferReturn(marker: string): Promise<Inferred> {
    const alias = `Endpoint_${marker}_Response`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        { file_path: ordersPath, line_number: lineOf(marker), infer_kind: 'function_return', alias },
      ],
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, `${marker} must answer, got ${JSON.stringify(res)}`);
    return inferred;
  }

  /** The stated answer is the abstain of carrick#2200, word for word. */
  function assertAbstain(inferred: Inferred, sentences: string[]): void {
    assert.strictEqual(inferred.type_string.trim(), 'unknown');
    assert.strictEqual(inferred.is_explicit, false);
    assert.deepStrictEqual(inferred.any_provenance, [
      { path: '', kind: 'unknown', reason: 'serialised_body_unread', detail: sentences.join(' ') },
    ]);
  }

  function spanTexts(inferred: Inferred): string[] {
    return (inferred.read_spans ?? []).map((span) => {
      assert.strictEqual(span.file_path, ordersPath);
      return ORDERS_TS.slice(span.start, span.end);
    });
  }

  it('names the read success send beside a ready-made reply passed through', async () => {
    const inferred = await inferReturn('GUARDED');
    assertAbstain(inferred, [PREBUILT]);
    assert.deepStrictEqual(spanTexts(inferred), ['summary']);
  });

  it('names a send whose value holds unknown inside it', async () => {
    const inferred = await inferReturn('HOLE');
    assertAbstain(inferred, [PREBUILT, TOP_TYPE]);
    assert.deepStrictEqual(spanTexts(inferred), ['{ id: found.id, extras }']);
  });

  it('names nothing when no branch sends a value it found', async () => {
    const inferred = await inferReturn('NONE_READ');
    assertAbstain(inferred, [PREBUILT]);
    assert.strictEqual(inferred.read_spans, undefined);
  });

  it('names nothing for a value typed unknown itself', async () => {
    const inferred = await inferReturn('BARE_UNKNOWN');
    assertAbstain(inferred, [PREBUILT, TOP_TYPE]);
    assert.strictEqual(inferred.read_spans, undefined);
  });
});
