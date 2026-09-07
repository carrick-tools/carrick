/**
 * Regression for carrick#768: a route that states its contract with
 * `satisfies X` lost the NAME `X` once the repo's dependencies were installed.
 *
 * The anchor (`primary_type_symbol`) was read off the RESOLVED type of the
 * annotation — `getSymbol() || getAliasSymbol()`. That works for an interface,
 * whose resolved type carries its own symbol. It does not work for the shape
 * most schema-first codebases write:
 *
 *     export type OrderBody = Inferred<typeof OrderSchema>;
 *
 * The alias resolves to an INSTANTIATED type. TypeScript keeps no alias symbol
 * on it, so `getSymbol()` answers the synthetic `__type` and the anchor came
 * back null — the route printed a correct shape that no reader could trace to
 * an importable name.
 *
 * On a bare checkout the same route DID report the name, for the wrong reason:
 * the annotated type did not resolve at all, so the unresolved reference's own
 * identifier survived as the symbol. Installing the dependencies is what
 * "lost" it. The declaration file is inside the repo either way (TypeScript
 * realpaths a workspace symlink), so nothing here is about `node_modules`.
 *
 * The fix reads the anchor from the ANNOTATION AS WRITTEN: the type node names
 * a type, and that name is exactly "the type a consumer imports", whether or
 * not the compiler kept an alias symbol on what it resolves to.
 *
 * Deliberate limits, one negative case each:
 *  - a generic instantiation (`satisfies Envelope<Order>`) names the wrapper,
 *    not the payload, so it anchors nothing;
 *  - a lib global (`satisfies Record<string, number>`) is not an importable
 *    contract;
 *  - an inline object annotation (`satisfies { id: string }`) names nothing.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const CONTRACTS_TS = `export interface SchemaShape<Out> {
  readonly _output: Out;
}

export type Inferred<S> = S extends SchemaShape<infer O> ? O : never;

export declare const OrderSchema: SchemaShape<{ id: string; total: number }>;

/** The shape carrick#768 is about: an alias to an INSTANTIATED type. */
export type OrderBody = Inferred<typeof OrderSchema>;

export interface Envelope<T> {
  data: T;
}
`;

const ROUTE_TS = `import { type OrderBody, type Envelope } from "./contracts.js";

declare function reply(body: unknown, init?: { status: number }): Response;

export async function loadOrder() {
  if (Math.random() > 0.5) {
    return reply({ error: "Not found" }, { status: 404 });
  }

  return reply({ id: "ord_1", total: 10 } satisfies OrderBody);
}

export async function loadOrderList() {
  return reply([{ id: "ord_1", total: 10 }] satisfies OrderBody[]);
}

export async function loadOrderCast() {
  return reply({ id: "ord_1", total: 10 } as OrderBody);
}

export async function loadEnvelope() {
  return reply({ data: { id: "ord_1", total: 10 } } satisfies Envelope<OrderBody>);
}

export async function loadRecord() {
  return reply({ totals: 1 } satisfies Record<string, number>);
}

export async function loadInline() {
  return reply({ id: "ord_1" } satisfies { id: string });
}
`;

function lineOf(marker: string): number {
  const idx = ROUTE_TS.split('\n').findIndex((l) => l.includes(marker));
  assert.ok(idx >= 0, `fixture must contain: ${marker}`);
  return idx + 1;
}

const ORDER_LINE = lineOf('export async function loadOrder(');
const LIST_LINE = lineOf('export async function loadOrderList');
const CAST_LINE = lineOf('export async function loadOrderCast');
const ENVELOPE_LINE = lineOf('export async function loadEnvelope');
const RECORD_LINE = lineOf('export async function loadRecord');
const INLINE_LINE = lineOf('export async function loadInline');

interface InferShape {
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    primary_type_symbol?: string;
    primary_type_symbol_source?: string;
    array_depth?: number;
  }>;
  errors?: string[];
}

function collapse(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

describe('carrick#768 stated-annotation anchor symbol', () => {
  let repoDir: string;
  let client: SidecarClient;
  let routePath: string;
  let contractsPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-768-'));
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
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    contractsPath = path.join(repoDir, 'src', 'contracts.ts');
    routePath = path.join(repoDir, 'src', 'route.ts');
    fs.writeFileSync(contractsPath, CONTRACTS_TS);
    fs.writeFileSync(routePath, ROUTE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function inferReturn(alias: string, line: number) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: routePath,
          line_number: line,
          infer_kind: 'function_return',
          alias,
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('names the stated contract whose resolved type kept no alias symbol', async () => {
    const inferred = await inferReturn('Endpoint_Order_Response', ORDER_LINE);
    assert.ok(inferred, 'must resolve a response contract');
    // The SHAPE is unchanged: the literal is what the route serialises.
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ id: string; total: number; }'
    );
    assert.strictEqual(inferred.is_explicit, true);
    // The NAME is the point of carrick#768.
    assert.strictEqual(inferred.primary_type_symbol, 'OrderBody');
    assert.ok(
      inferred.primary_type_symbol_source?.endsWith('contracts.ts'),
      `the anchor must point at its declaring file, got: ${inferred.primary_type_symbol_source}`
    );
  });

  it('peels array levels off the annotation and reports the depth', async () => {
    const inferred = await inferReturn('Endpoint_List_Response', LIST_LINE);
    assert.ok(inferred);
    assert.strictEqual(inferred.primary_type_symbol, 'OrderBody');
    assert.strictEqual(inferred.array_depth, 1);
  });

  it('reads an `as` cast the same way as `satisfies`', async () => {
    const inferred = await inferReturn('Endpoint_Cast_Response', CAST_LINE);
    assert.ok(inferred);
    assert.strictEqual(inferred.primary_type_symbol, 'OrderBody');
  });

  it('leaves a resolved anchor alone: the written name never overrides it', async () => {
    // `Envelope<OrderBody>` resolves to a type that DOES carry a symbol, so
    // the pre-existing resolved-type anchor answers and the annotation reader
    // is never consulted. It would have rejected the generic instantiation
    // anyway — `Envelope` names the wrapper, not the payload — which is what
    // keeps the fallback from widening an anchor the resolver already fixed.
    const inferred = await inferReturn('Endpoint_Envelope_Response', ENVELOPE_LINE);
    assert.ok(inferred);
    assert.strictEqual(inferred.primary_type_symbol, 'Envelope');
    assert.notStrictEqual(inferred.primary_type_symbol, 'OrderBody');
  });

  it('anchors nothing on a lib global', async () => {
    const inferred = await inferReturn('Endpoint_Record_Response', RECORD_LINE);
    assert.ok(inferred);
    assert.strictEqual(inferred.primary_type_symbol, undefined);
  });

  it('anchors nothing on an inline object annotation', async () => {
    const inferred = await inferReturn('Endpoint_Inline_Response', INLINE_LINE);
    assert.ok(inferred);
    assert.strictEqual(inferred.primary_type_symbol, undefined);
  });
});
