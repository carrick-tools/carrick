/**
 * carrick#371: a wrapped route-handler's literal return ENVELOPE must never be
 * emitted as a producer response contract.
 *
 * A Next.js-style producer `export const GET = withApiWrapper({ handler: () =>
 * ({ response: Response.json(data), error: undefined }) })` resolves its
 * handler return to `{ response: Response; error?: undefined } | { ...; error:
 * Error }` — framework transport machinery, not the JSON payload. Captured as
 * the response type, it manufactures a FALSE compat mismatch against the
 * consumer that anchors the real payload (`{ id; name }`). This is the
 * producer-side mirror of the consumer `machineryIndicators` guard.
 *
 * The fix is two fail-closed guards, both exercised here:
 *  1. The v1 inferrer (`buildFunctionReturnInferredType`) abstains when the
 *     resolved return IS or CONTAINS machinery — so no Literal anchor ever
 *     carries the envelope text (the primary production path).
 *  2. The capture Infer fallback (`resolveAnchor`) degrades to `unknown` when
 *     its resolved type IS or CONTAINS machinery — so the wrapper FUNCTION type
 *     `(req) => Promise<Response>` that the fallback resolves to never surfaces.
 *
 * Acceptable outcomes: the unwrapped payload, or `unknown`. Forbidden: any
 * concrete verdict off the machinery. Detection is structural + origin-gated;
 * no framework name is hardcoded.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import type {
  CaptureStubResult,
  CheckPairSpec,
  CheckVerdict,
} from '../src/capture/api.js';
import { SidecarClient } from './helpers.js';

const ROUTE_TS = `interface Payload { id: number; name: string; }
declare function fetchPayload(): Promise<Payload>;
declare function withApiWrapper(cfg: {
  handler: () => Promise<
    { response: Response; error?: undefined } | { response: Response; error: Error }
  >;
}): (req: Request) => Promise<Response>;

export const GET = withApiWrapper({
  handler: async () => {
    const data = await fetchPayload();
    return { response: Response.json(data), error: undefined };
  },
});

export async function plainHandler(): Promise<Payload> {
  return { id: 1, name: "widget" };
}
`;

function lineOf(marker: string): number {
  const idx = ROUTE_TS.split('\n').findIndex((l) => l.includes(marker));
  assert.ok(idx >= 0, `fixture must contain: ${marker}`);
  return idx + 1;
}

const GET_LINE = lineOf('export const GET');
const PLAIN_LINE = lineOf('export async function plainHandler');

function writeRepo(repoDir: string): void {
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
  fs.writeFileSync(path.join(repoDir, 'src', 'route.ts'), ROUTE_TS);
}

// ===========================================================================
// Guard 1: the v1 inferrer abstains on the machinery envelope (Literal source).
// ===========================================================================

interface InferShape {
  status: string;
  inferred_types?: Array<{ alias: string; type_string: string }>;
  errors?: string[];
}

describe('carrick#371 v1 inference abstains on a wrapped response envelope', () => {
  let repoDir: string;
  let client: SidecarClient;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-371-v1-'));
    writeRepo(repoDir);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  it('does NOT emit the machinery envelope as an inferred response type', async () => {
    // Pre-fix: this returned `{ response: Response; error: undefined; }`, which
    // rode a Literal capture anchor into the producer surface.
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: 'envelope',
      requests: [
        {
          file_path: path.join(repoDir, 'src', 'route.ts'),
          line_number: GET_LINE,
          infer_kind: 'function_return',
          alias: 'Endpoint_GET_Response',
        },
      ],
    });
    const inferred = (res.inferred_types ?? []).find(
      (t) => t.alias === 'Endpoint_GET_Response'
    );
    // The alias resolves to nothing usable (abstained) — never the envelope.
    if (inferred) {
      assert.ok(
        !/response\s*:/.test(inferred.type_string) &&
          !/Response/.test(inferred.type_string),
        `must not carry the machinery envelope, got: ${inferred.type_string}`
      );
    } else {
      assert.ok(true, 'abstained (no inferred type for the envelope alias)');
    }
  });

  it('control: a plain payload return is still inferred (no over-abstain)', async () => {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: 'plain',
      requests: [
        {
          file_path: path.join(repoDir, 'src', 'route.ts'),
          line_number: PLAIN_LINE,
          infer_kind: 'function_return',
          alias: 'Endpoint_PLAIN_Response',
        },
      ],
    });
    const inferred = (res.inferred_types ?? []).find(
      (t) => t.alias === 'Endpoint_PLAIN_Response'
    );
    assert.ok(inferred, 'a plain payload handler must still resolve');
    assert.match(inferred!.type_string, /id/);
    assert.match(inferred!.type_string, /name/);
  });
});

// ===========================================================================
// Guard 2: the capture Infer fallback degrades the machinery type to unknown.
// ===========================================================================

describe('carrick#371 capture degrades a wrapped response envelope to unknown', () => {
  let repoDir: string;
  let outRoot: string;
  let result: CaptureStubResult;
  let surface: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-371-cap-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-371-stub-'));
    writeRepo(repoDir);
    result = captureStub({
      repoRoot: repoDir,
      serviceName: 'route-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: [
        // The FILE_BASED_ROUTE fallback shape once v1 abstains: a line-located
        // infer anchor. Its resolved type is the WRAPPER FUNCTION
        // `(req: Request) => Promise<Response>` — machinery in return position.
        {
          kind: 'infer',
          alias: 'Endpoint_GET_Response',
          source_file: 'src/route.ts',
          anchor_origin: 'deterministic-infer',
          line_number: GET_LINE,
        },
        // The envelope object literal itself: machinery in a direct property.
        {
          kind: 'infer',
          alias: 'Envelope_Response',
          source_file: 'src/route.ts',
          anchor_origin: 'deterministic-infer',
          expression_text: '{ response: Response.json(data), error: undefined }',
          unwrap: 'none',
        },
        // Control: a real payload object literal — captured as-is, not degraded.
        {
          kind: 'infer',
          alias: 'Payload_Response',
          source_file: 'src/route.ts',
          anchor_origin: 'deterministic-infer',
          expression_text: '{ id: 1, name: "widget" }',
          unwrap: 'none',
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(
      path.join(result.stub_dir, 'types', 'surface.d.ts'),
      'utf8'
    );
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  function record(alias: string) {
    const r = result.aliases.find((a) => a.alias === alias);
    assert.ok(r, `no alias record for ${alias}`);
    return r!;
  }

  /**
   * Full RHS of `export type <alias> = <rhs>;` — scoped to that one statement.
   * The terminating `;` is the one followed (after any whitespace) by the next
   * `export type` or end-of-file, NOT the first inner `;` — so a multi-member
   * object RHS (`{ id: number; name: string }`) is captured whole, never
   * truncated at `{ id: number`.
   */
  function rhsOf(alias: string): string {
    const m = surface.match(
      new RegExp(`export type ${alias} = ([\\s\\S]*?);(?=\\s*(?:export type |$))`)
    );
    assert.ok(m, `no surface line for ${alias}:\n${surface}`);
    return m![1];
  }

  it('degrades the line-anchored wrapper function type to unknown', () => {
    // Pre-fix this captured `(req: Request) => Promise<Response>`.
    const r = record('Endpoint_GET_Response');
    assert.strictEqual(r.serialization, 'structural_fallback');
    assert.ok(r.capture_failure_reason, 'must record a demotion reason');
    assert.match(r.capture_failure_reason!, /framework machinery/);
    assert.strictEqual(rhsOf('Endpoint_GET_Response'), 'unknown');
  });

  it('degrades the envelope object literal (machinery in a property) to unknown', () => {
    // Pre-fix this captured `{ response: Response; error: undefined; }`.
    const r = record('Envelope_Response');
    assert.strictEqual(r.serialization, 'structural_fallback');
    assert.match(r.capture_failure_reason!, /framework machinery/);
    assert.strictEqual(rhsOf('Envelope_Response'), 'unknown');
  });

  it('no machinery type text survives anywhere in the surface', () => {
    // Type-position machinery (never the `_Response` alias-name suffix).
    assert.ok(!/:\s*Response\b/.test(surface), surface);
    assert.ok(!/Promise<Response>/.test(surface), surface);
    assert.ok(!/req:\s*Request\b/.test(surface), surface);
  });

  it('control: a real payload literal is captured as-is (no over-abstain)', () => {
    const r = record('Payload_Response');
    assert.strictEqual(r.self_check, 'ok', r.self_check_detail);
    // rhsOf captures the WHOLE multi-member object (both members past the first
    // inner `;`): the truncating helper this replaced saw only `{ id: number`.
    const rhs = rhsOf('Payload_Response');
    assert.match(rhs, /id/);
    assert.match(rhs, /name/);
    assert.ok(!/Payload_Response = unknown;/.test(surface), surface);
  });
});

// ===========================================================================
// End-to-end: the captured producer never yields a false compat mismatch.
// ===========================================================================

describe('carrick#371 check: wrapped response envelope yields no false mismatch', () => {
  let repoDir: string;
  let outRoot: string;
  let workRoot: string;
  let verdicts: Map<string, CheckVerdict>;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-371-e2e-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-371-e2e-stub-'));
    workRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-371-e2e-ws-'));
    writeRepo(repoDir);

    // Producer: capture the wrapped route via the real FILE_BASED_ROUTE
    // fallback anchor (line-located infer). Post-fix -> `unknown`.
    const producer = captureStub({
      repoRoot: repoDir,
      serviceName: 'producer',
      outDir: path.join(outRoot, 'producer'),
      anchors: [
        {
          kind: 'infer',
          alias: 'Wrapped_Sent',
          source_file: 'src/route.ts',
          anchor_origin: 'deterministic-infer',
          line_number: GET_LINE,
        },
      ],
    });
    assert.strictEqual(producer.success, true, JSON.stringify(producer.errors));

    // Consumer: a hand-written stub anchoring the REAL payload the handler
    // sends, plus the raw envelope to document the pre-fix false mismatch.
    const consumerDir = path.join(outRoot, 'consumer');
    fs.mkdirSync(path.join(consumerDir, 'types'), { recursive: true });
    fs.writeFileSync(
      path.join(consumerDir, 'package.json'),
      JSON.stringify({
        name: '@carrick/consumer',
        version: '0.0.0-carrick',
        private: true,
        types: './types/surface.d.ts',
      }) + '\n'
    );
    fs.writeFileSync(
      path.join(consumerDir, 'types', 'surface.d.ts'),
      [
        'export type Wrapped_Exp = { id: number; name: string };',
        'export type RawEnvelope_Sent = { response: Response; error?: undefined } | { response: Response; error: Error };',
        'export type RawEnvelope_Exp = { id: number; name: string };',
      ].join('\n') + '\n'
    );

    const pairs: CheckPairSpec[] = [
      // The fix: producer captured from the fixture vs the real payload.
      {
        pair_key: 'wrapped',
        protocol: 'http',
        type_kind: 'response',
        producer: { service_name: 'producer', alias: 'Wrapped_Sent' },
        consumer: { service_name: 'consumer', alias: 'Wrapped_Exp' },
      },
      // Control documenting the bug: the raw machinery envelope vs the payload
      // IS an incompatible — exactly the false verdict the fix prevents.
      {
        pair_key: 'raw-envelope',
        protocol: 'http',
        type_kind: 'response',
        producer: { service_name: 'consumer', alias: 'RawEnvelope_Sent' },
        consumer: { service_name: 'consumer', alias: 'RawEnvelope_Exp' },
      },
    ];

    const res = await runCheck({
      stubs: [
        { service_name: 'producer', stub_dir: producer.stub_dir },
        { service_name: 'consumer', stub_dir: consumerDir },
      ],
      pairs,
      workspaceRoot: workRoot,
    });
    assert.strictEqual(res.success, true, JSON.stringify(res.errors));
    assert.strictEqual(res.install_ok, true, JSON.stringify(res.errors));
    verdicts = new Map(res.verdicts.map((v) => [v.pair_key, v]));
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
    fs.rmSync(workRoot, { recursive: true, force: true });
  });

  it('the wrapped producer never reports a mismatch (unverifiable, not incompatible)', () => {
    // Pre-fix the producer surface was the machinery type and this was
    // `incompatible` — the stable false verdict carrick#371 reports.
    const v = verdicts.get('wrapped')!;
    assert.notStrictEqual(v.bucket, 'incompatible', v.diagnostic);
    assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
  });

  it('control: the raw envelope vs the payload IS the incompatible the fix avoids', () => {
    assert.strictEqual(verdicts.get('raw-envelope')!.bucket, 'incompatible');
  });
});
