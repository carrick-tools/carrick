/**
 * Regression for carrick#166 part B: a union whose members did not all resolve
 * must not answer with the ones that did.
 *
 * A handler that returns several branches resolves to a union. Each member is
 * unwrapped in turn, and a member that a rule verifies as transport but can
 * recover no payload from collapses to `unknown`. The join then dropped every
 * `unknown` — so a partial resolution could not read as a composite type — and
 * published whatever survived AS THE CONTRACT.
 *
 * Live shape (`POST /api/v1/authorization-code`): the awaited return is
 *
 *     TypedResponse<{ error: string }>
 *   | TypedResponse<CreateAuthorizationCodeResponse>
 *   | { status: number; body: string }
 *
 * The two wrapper members are intersections whose lib half IS the DOM
 * `Response`, which the extraction config verifies as machinery and finds no
 * payload in. Both collapsed, the bare early return survived, and the endpoint
 * published `{ status: number; body: string }` — a status envelope from one
 * guard branch, stated as the whole contract, with `is_explicit: false` and no
 * signal that anything was dropped.
 *
 * A contract known to be partial is a false match, and a false match is worse
 * than the `any` it replaced. So a partial union answers `unknown` and carries
 * the reason, naming how many branches were unread and what they were.
 *
 * Three negatives, because the rule must not swallow the cases it does not
 * describe: a union with no dropped members still prints every branch, a
 * single wrapper still prints its payload, and a union whose members ALL
 * collapse is unchanged — that verdict is the carrick#631 recovery's cue to
 * read the handler's returned arguments, and it must keep getting its turn.
 *
 * Reading the payload out of a wrapper that carries it in a member, and
 * unioning literal `new Response(...)` / `redirect(...)` branches, are the
 * other two halves of carrick#166 and are deliberately not done here.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

/** A transport wrapper with a readable payload, declared like a dependency. */
const CARRIER_DTS = `export interface Carrier<T> {
  payload: T;
}
export declare function carry<T>(value: T): Carrier<T>;
`;

const ROUTE_TS = `import { carry, type Carrier } from "carrier-runtime";

interface OkBody {
  id: string;
}

interface ErrBody {
  error: string;
}

declare const flag: boolean;
declare const okBody: OkBody;
declare const errBody: ErrBody;

export async function partialUnion() {
  if (flag) {
    return new Response(null, { status: 405 });
  }
  return carry<OkBody>(okBody);
}

export async function fullUnion() {
  if (flag) {
    return carry<OkBody>(okBody);
  }
  return carry<ErrBody>(errBody);
}

export async function singleWrapper() {
  return carry<OkBody>(okBody);
}

export async function sameTwice(): Promise<Carrier<OkBody>> {
  if (flag) {
    return carry<OkBody>(okBody);
  }
  return carry<OkBody>(okBody);
}

export async function allCollapsed() {
  if (flag) {
    return new Response(null, { status: 405 });
  }
  return new Response(null, { status: 204 });
}
`;

/**
 * The rules a scan carries for this shape: one that verifies the DOM
 * `Response` as transport and recovers nothing from it (it has no payload
 * generic), and one that reads the carrier's payload out of its generic.
 */
const EXTRACTION_CONFIG = {
  rules: [
    {
      wrapperSymbols: ['Response'],
      machineryIndicators: ['headers', 'status', 'statusText', 'ok', 'body'],
      originModuleGlobs: ['typescript/lib/*'],
      unwrapRecursively: false,
    },
    {
      wrapperSymbols: ['Carrier'],
      originModuleGlobs: ['carrier-runtime'],
      payloadGenericIndex: 0,
      unwrapRecursively: false,
    },
  ],
};

function lineOf(marker: string): number {
  const idx = ROUTE_TS.split('\n').findIndex((l) => l.includes(marker));
  assert.ok(idx >= 0, `fixture must contain: ${marker}`);
  return idx + 1;
}

const PARTIAL_LINE = lineOf('export async function partialUnion');
const FULL_LINE = lineOf('export async function fullUnion');
const SINGLE_LINE = lineOf('export async function singleWrapper');
const SAME_TWICE_LINE = lineOf('export async function sameTwice');
const ALL_COLLAPSED_LINE = lineOf('export async function allCollapsed');

interface Provenance {
  path: string;
  kind: string;
  reason: string;
  detail?: string;
}

interface InferShape {
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    any_provenance?: Provenance[];
  }>;
  errors?: string[];
}

function collapse(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

describe('carrick#166 part B: a partial union never answers with its survivors', () => {
  let repoDir: string;
  let client: SidecarClient;
  let routePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-166b-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const carrierDir = path.join(repoDir, 'node_modules', 'carrier-runtime');
    fs.mkdirSync(carrierDir, { recursive: true });
    fs.writeFileSync(
      path.join(carrierDir, 'package.json'),
      JSON.stringify({ name: 'carrier-runtime', version: '1.0.0', types: './index.d.ts' })
    );
    fs.writeFileSync(path.join(carrierDir, 'index.d.ts'), CARRIER_DTS);
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
    routePath = path.join(repoDir, 'src', 'route.ts');
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
      extraction_config: EXTRACTION_CONFIG,
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('answers unknown when one branch resolved and another did not', async () => {
    const inferred = await inferReturn('Endpoint_Partial_Response', PARTIAL_LINE);
    assert.ok(inferred, 'the request must answer, with unknown rather than nothing');
    assert.strictEqual(
      collapse(inferred.type_string),
      'unknown',
      'the readable branch alone is a partial contract, not the contract'
    );
    assert.strictEqual(inferred.is_explicit, false);
  });

  it('names the unread branches in the reason it carries', async () => {
    const inferred = await inferReturn('Endpoint_Partial_Reason', PARTIAL_LINE);
    assert.ok(inferred);
    const provenance = inferred.any_provenance ?? [];
    assert.strictEqual(provenance.length, 1, 'the unknown must state why it is unknown');
    assert.strictEqual(provenance[0].path, '');
    assert.strictEqual(provenance[0].kind, 'unknown');
    assert.strictEqual(provenance[0].reason, 'machinery_envelope');
    assert.match(
      provenance[0].detail ?? '',
      /Response/,
      `the reason must name the branch that could not be read, got: ${provenance[0].detail}`
    );
    assert.ok(
      !/\//.test(provenance[0].detail ?? ''),
      `the reason must carry no path, got: ${provenance[0].detail}`
    );
  });

  it('still prints every branch of a union that dropped none', async () => {
    const inferred = await inferReturn('Endpoint_Full_Response', FULL_LINE);
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), 'OkBody | ErrBody');
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('still prints a single wrapper\'s payload', async () => {
    const inferred = await inferReturn('Endpoint_Single_Response', SINGLE_LINE);
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), 'OkBody');
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('still prints a union whose members resolve to one payload', async () => {
    const inferred = await inferReturn('Endpoint_Same_Response', SAME_TWICE_LINE);
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), 'OkBody');
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('leaves an all-collapsed union to the return-statement recovery', async () => {
    // Every branch is verified transport, so the union carries no contract at
    // all — the carrick#631 verdict, whose recovery reads the handler's
    // returned arguments. Nothing here hands a payload to anything, so it
    // abstains. What must NOT happen is this route acquiring a partial-union
    // reason: there is nothing partial about it.
    const inferred = await inferReturn('Endpoint_AllCollapsed_Response', ALL_COLLAPSED_LINE);
    if (inferred) {
      assert.strictEqual(collapse(inferred.type_string), 'unknown');
      for (const p of inferred.any_provenance ?? []) {
        assert.ok(
          !/branch/.test(p.detail ?? ''),
          `an all-collapsed union is not a partial union, got: ${p.detail}`
        );
      }
    }
  });
});
