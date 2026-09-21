/**
 * Regression for carrick#1017: the fetch `Response` wrapper was published as a
 * route's response type.
 *
 * `Response.json(entry)`, `new Response(JSON.stringify(entry))` and
 * `ctx.json(entry)` all evaluate to the platform `Response`. Reading the type
 * of the returned expression therefore answers the transport object —
 * `headers`, `ok`, `redirected`, `status`, `json(): Promise<any>` — and every
 * consumer that reads the real body is told its type is missing those members.
 * The body is in the ARGUMENT of the response call, so that is where it is
 * read from, per branch of a conditional, with a branch whose options object
 * states a >= 400 status dropped as the error body it is.
 *
 * Two halves of the same false mismatch:
 *
 *  (a) PRODUCER. The three body shapes above, plus a route that hands back a
 *      response it did not build — there the body cannot be determined, and
 *      the row must stay unresolved rather than publish the wrapper. A verdict
 *      is impossible either way; a WRONG verdict is not.
 *
 *  (b) CONSUMER. `const res = await fetch(url); if (res.status === 404) return
 *      null; return (await res.json()) as Entry` recorded the expected
 *      response type as `boolean` — the def-use walk ended on the status
 *      comparison beside the call, and the producer's real body was then
 *      reported as not assignable to it.
 *
 * The runtime declarations live where Carrick materialises them for a non-Node
 * runtime (`.carrick/deno/<hash>/runtime.d.ts`), which is the origin that made
 * this live: the machinery gate admits a `lib.*.d.ts` and `node_modules`, and
 * that file is neither, so nothing recognised the platform `Response` at all.
 * No framework name is matched anywhere — only the standard response
 * constructor, the json-body call shape, and Carrick's own artefact path.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

/** Ambient runtime globals, as `deno types` emits them: no imports, no exports. */
const RUNTIME_DTS = `interface ResponseInit {
  status?: number;
  statusText?: string;
  headers?: Record<string, string>;
}

interface Body {
  readonly body: unknown;
  readonly bodyUsed: boolean;
  arrayBuffer(): Promise<ArrayBuffer>;
  json(): Promise<any>;
  text(): Promise<string>;
}

interface Response extends Body {
  readonly headers: Record<string, string>;
  readonly ok: boolean;
  readonly redirected: boolean;
  readonly status: number;
  readonly statusText: string;
  readonly url: string;
  clone(): Response;
}

declare var Response: {
  new (body?: unknown, init?: ResponseInit): Response;
  json(data: unknown, init?: ResponseInit): Response;
};

declare function fetch(input: string, init?: unknown): Promise<Response>;

/** A context object that both reads the request and sends the response. */
interface RouteContext {
  json(data: unknown, init?: ResponseInit): Response;
}
`;

const SERVICE_TS = `export interface LedgerEntry {
  id: string;
  account: string;
  total: number;
}

export interface Summary {
  count: number;
  total: number;
}

export interface AuditRecord {
  actor: string;
  action: string;
}

declare function getEntry(id: string): LedgerEntry | undefined;
declare function readSummary(): Summary;
declare function readAudit(): AuditRecord;
declare const ctx: RouteContext;
declare function forward(url: string): Promise<Response>;

export function entryRoute(id: string) {
  const entry = getEntry(id);
  return entry ? Response.json(entry) : Response.json({ error: "not found" }, { status: 404 });
}

export function summaryRoute() {
  return new Response(JSON.stringify(readSummary()), {
    headers: { "content-type": "application/json" },
  });
}

export function auditRoute() {
  return ctx.json(readAudit());
}

export async function proxyRoute() {
  return await forward("/upstream");
}

export async function fetchEntry(id: string): Promise<LedgerEntry | null> {
  const response = await fetch("/entries/" + id);
  if (response.status === 404) return null;
  return (await response.json()) as LedgerEntry;
}

export interface Outcome {
  status: number;
  body?: string;
}

declare function runOutcome(): Promise<Outcome>;
declare function send(body: unknown, meta: unknown): Response;

export async function outcomeRoute() {
  const result = await runOutcome();
  return new Response(result.body ?? "", { status: result.status });
}

export async function summaryStatusRoute() {
  const result = await runOutcome();
  return new Response(JSON.stringify(readSummary()), { status: result.status });
}

export function queuedRoute() {
  return send("accepted", { status: "queued" });
}
`;

/** 1-based lines in SERVICE_TS, read off the source above. */
const TERNARY_LINE = 25;
const NEW_RESPONSE_LINE = 29;
const CONTEXT_JSON_LINE = 35;
const PROXY_LINE = 39;
const FETCH_LINE = 43;
const VARIABLE_STATUS_LINE = 56;
const VARIABLE_STATUS_WITH_BODY_LINE = 61;
const STRING_STATUS_LINE = 66;

/** The live extraction config for a service on this runtime (ticket evidence). */
const EXTRACTION_CONFIG = {
  rules: [
    {
      wrapperSymbols: ['Response'],
      machineryIndicators: [
        'status',
        'statusText',
        'headers',
        'body',
        'bodyUsed',
        'ok',
        'redirected',
        'type',
        'url',
      ],
      originModuleGlobs: ['typescript/lib/*', '@types/node/*'],
      unwrapRecursively: false,
    },
  ],
};

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    primary_type_symbol?: string;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1017: the json body, never the Response wrapper', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1017-'));
    const runtimeDir = path.join(repoDir, '.carrick', 'deno', 'a1b2c3d4e5f60718');
    fs.mkdirSync(runtimeDir, { recursive: true });
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(runtimeDir, 'runtime.d.ts'), RUNTIME_DTS);
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          // No `dom`: the runtime declarations ARE the platform here, exactly
          // as they are for a service Carrick prepared for a non-Node runtime.
          lib: ['es2022'],
          skipLibCheck: true,
        },
        include: ['src', '.carrick/deno/*/runtime.d.ts'],
      })
    );
    servicePath = path.join(repoDir, 'src', 'service.ts');
    fs.writeFileSync(servicePath, SERVICE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /**
   * The locator a scan really sends: the model's expression text plus the line
   * it sits on. `expressionText` is omitted only to exercise the line-only
   * fallback.
   */
  async function infer(
    alias: string,
    line: number,
    kind: string,
    expressionText?: string
  ) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: servicePath,
          line_number: line,
          infer_kind: kind,
          alias,
          ...(expressionText
            ? { expression_text: expressionText, expression_line: line }
            : {}),
        },
      ],
      extraction_config: EXTRACTION_CONFIG,
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('reads the payload of Response.json(x) through a conditional, dropping the 404 branch', async () => {
    const inferred = await infer(
      'Endpoint_Entry_Response',
      TERNARY_LINE,
      'response_body',
      'Response.json(entry)'
    );
    assert.ok(inferred, 'must resolve the entry body, not abstain');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ id: string; account: string; total: number; }'
    );
    assert.ok(
      !/headers|redirected|bodyUsed/.test(inferred.type_string),
      `the transport wrapper must never be the contract, got: ${inferred.type_string}`
    );
    assert.ok(
      !/error/.test(inferred.type_string),
      `the 404 branch states an error body, not the contract, got: ${inferred.type_string}`
    );
  });

  it('resolves the same body from a line-only locator', async () => {
    // The scan falls back to the line when the model reports no expression.
    const inferred = await infer('Endpoint_Entry_Response_Line', TERNARY_LINE, 'response_body');
    assert.ok(inferred, 'must resolve the entry body from the line alone');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ id: string; account: string; total: number; }'
    );
  });

  it('reads the serialized payload of new Response(JSON.stringify(x))', async () => {
    const inferred = await infer(
      'Endpoint_Summary_Response',
      NEW_RESPONSE_LINE,
      'response_body',
      'new Response(JSON.stringify(readSummary()))'
    );
    assert.ok(inferred, 'must resolve the summary body, not abstain');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ count: number; total: number; }'
    );
  });

  it('reads the payload of a context json call', async () => {
    const inferred = await infer(
      'Endpoint_Audit_Response',
      CONTEXT_JSON_LINE,
      'response_body',
      'ctx.json(readAudit())'
    );
    assert.ok(inferred, 'must resolve the audit body, not abstain');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ actor: string; action: string; }'
    );
  });

  it('leaves a response it did not build unresolved rather than publishing the wrapper', async () => {
    const inferred = await infer(
      'Endpoint_Proxy_Response',
      PROXY_LINE,
      'response_body',
      'forward("/upstream")'
    );
    if (inferred) {
      assert.ok(
        !/headers|redirected|bodyUsed/.test(inferred.type_string),
        `no body is determinable here, so nothing may be published; got: ${inferred.type_string}`
      );
      assert.strictEqual(collapse(inferred.type_string), 'unknown');
    } else {
      assert.ok(true, 'abstained: no verdict is possible, and none is claimed');
    }
  });

  it('never publishes the init object when the status beside the body is a variable', async () => {
    // The shape a file-based route writes when the status travels with the
    // result: `new Response(<body>, { status: result.status })`. The init
    // object states HOW to send, never WHAT is sent — whether the status is a
    // literal code or a value the source does not fix. The body here is a
    // string, which this layer does not publish as a contract, so the honest
    // answer is an abstention.
    const inferred = await infer(
      'Endpoint_Outcome_Response',
      VARIABLE_STATUS_LINE,
      'function_return'
    );
    assert.ok(
      !inferred || collapse(inferred.type_string) !== '{ status: number; }',
      `the ResponseInit object is not the response contract, got: ${inferred?.type_string}`
    );
    assert.ok(
      !inferred,
      `the body is a string, so nothing object-shaped may be published; got: ${inferred?.type_string}`
    );
  });

  it('still reads the body when a variable status travels beside it', async () => {
    // Control for the case above: skipping the init object must not cost the
    // body when there IS one.
    const inferred = await infer(
      'Endpoint_SummaryStatus_Response',
      VARIABLE_STATUS_WITH_BODY_LINE,
      'function_return'
    );
    assert.ok(inferred, 'must resolve the summary body, not abstain');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ count: number; total: number; }'
    );
  });

  it('keeps an object whose status is not status-shaped as a payload', async () => {
    // The negative of the rule: an argument beside the body counts as init
    // only when it states init — an HTTP status code, fixed or variable, or
    // headers. `{ status: "queued" }` states neither, so it stays a payload
    // and the init test must not have been widened to any `status` member.
    const inferred = await infer(
      'Endpoint_Queued_Response',
      STRING_STATUS_LINE,
      'function_return'
    );
    assert.ok(inferred, 'must resolve the queued payload, not abstain');
    assert.strictEqual(collapse(inferred.type_string), '{ status: string; }');
  });

  it('takes the body a consumer reads, not the status check beside the call', async () => {
    const inferred = await infer(
      'Endpoint_Entry_Response_Call',
      FETCH_LINE,
      'call_result',
      'fetch("/entries/" + id)'
    );
    assert.ok(inferred, 'must resolve the expected body');
    assert.notStrictEqual(
      collapse(inferred.type_string),
      'boolean',
      'the status comparison is boolean by construction and is never the payload'
    );
    assert.ok(
      /LedgerEntry|account/.test(inferred.type_string),
      `the expected body is what the call site reads out of the response, got: ${inferred.type_string}`
    );
  });
});
