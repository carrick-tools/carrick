/**
 * Gap-regression tests for the type inferrer.
 *
 * Each test replays a request shape the Rust orchestrator actually sends
 * (see file_orchestrator.rs `push_infer`: text locator from the LLM,
 * span locator from the SWC candidate, or line-only anchors for
 * file-based routes) against test/fixtures/sample-repo/src/gap-cases.ts
 * and asserts the *correct* behavior:
 *
 * 1. An identifier locator with a drifted line must fail (→ unknown
 *    downstream) rather than substring-bind to a different identifier
 *    and return a confidently wrong type.
 * 2. A span locator pointing at the endpoint *registration* call (the
 *    fallback when the LLM emits no payload expression) must not type
 *    the route path literal as the response payload.
 * 3. Line-anchored `function_return` requests — the only shape sent for
 *    file-based routes (Next.js app router etc.) — must be accepted, not
 *    rejected by the protocol validator.
 * 4. One invalid item in an infer batch must not poison the whole batch.
 * 5. A union of Promises must unwrap per member, not produce a mangled
 *    type string.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';

const GAP_FIXTURE = path.join(FIXTURES_PATH, 'src/gap-cases.ts');
const GAP_FIXTURE_SOURCE = fs.readFileSync(GAP_FIXTURE, 'utf-8');

// Line numbers in gap-cases.ts (load-bearing; see fixture header comment).
const LINE_USERS_CSV_DECL = 30; // drifted locator points here; real payload is line 38-39
const LINE_RES_JSON_USERS = 39;
const LINE_RETURN_STYLE_HANDLER = 51;
const LINE_LOAD_USERS = 64;

/** Byte span of `text` in the fixture (ASCII-only file, so byte == char offsets). */
function spanOf(text: string): { start: number; end: number; line: number } {
  const start = GAP_FIXTURE_SOURCE.indexOf(text);
  assert.ok(start >= 0, `fixture must contain: ${text}`);
  assert.strictEqual(
    GAP_FIXTURE_SOURCE.indexOf(text, start + 1),
    -1,
    `fixture must contain exactly one occurrence of: ${text}`
  );
  const line = GAP_FIXTURE_SOURCE.slice(0, start).split('\n').length;
  return { start, end: start + text.length, line };
}

interface InferResponseShape {
  request_id: string;
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    infer_kind: string;
    is_explicit: boolean;
    any_provenance?: Array<{ path: string; reason: string }>;
  }>;
  errors?: string[];
}

describe('Type inferrer gap regressions', () => {
  let client: SidecarClient;

  before(async () => {
    client = new SidecarClient();
    await client.start();
    await client.send({
      action: 'init',
      request_id: 'gap-init',
      repo_root: FIXTURES_PATH,
    });
  });

  after(async () => {
    await client.stop();
  });

  it('does not substring-bind an identifier locator to a different identifier', async () => {
    // The analysis reported payload expression `users` at a drifted line
    // where only `usersCsv` exists. Binding to `usersCsv` would emit
    // `string` as the response type — confidently wrong. The correct
    // outcome is no inferred type (downstream pads the alias to unknown).
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-substring',
      requests: [
        {
          file_path: GAP_FIXTURE,
          line_number: LINE_USERS_CSV_DECL,
          expression_text: 'users',
          expression_line: LINE_USERS_CSV_DECL,
          infer_kind: 'response_body',
          alias: 'GapSubstringResponse',
        },
      ],
    });

    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'GapSubstringResponse'
    );
    assert.strictEqual(
      inferred,
      undefined,
      `expected no inferred type for a drifted identifier locator, got ${inferred?.type_string}`
    );
    assert.ok(
      response.errors && response.errors.length > 0,
      'expected an error explaining the failed locator'
    );
  });

  it('still resolves an identifier locator with an accurate line', async () => {
    // Same expression, correct line — the exact-match path must keep working.
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-substring-control',
      requests: [
        {
          file_path: GAP_FIXTURE,
          line_number: LINE_RES_JSON_USERS,
          expression_text: 'users',
          expression_line: LINE_RES_JSON_USERS,
          infer_kind: 'response_body',
          alias: 'GapSubstringControl',
        },
      ],
    });

    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'GapSubstringControl'
    );
    assert.ok(inferred, 'expected inferred type for accurate locator');
    // #257/#240: the named payload is now expanded structurally, not the
    // dangling bare `User[]`.
    assert.strictEqual(
      inferred.type_string,
      '{ id: number; name: string; }[]',
      `expected structural User[], got ${inferred.type_string}`
    );
  });

  it('does not type the route path literal when the span covers a registration call', async () => {
    // No payload expression from the LLM → the orchestrator falls back to
    // the SWC span of the *registration* call. Drilling into its first
    // argument would report `"/login-redirect"` as the response type.
    const regCall = spanOf(
      "app.get('/login-redirect', (_req, res) => {\n  res.redirect('/login');\n})"
    );
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-registration-span',
      requests: [
        {
          file_path: GAP_FIXTURE,
          line_number: regCall.line,
          span_start: regCall.start,
          span_end: regCall.end,
          infer_kind: 'response_body',
          alias: 'GapRegistrationResponse',
        },
      ],
    });

    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'GapRegistrationResponse'
    );
    // carrick#1913: answered, and with a reason, never left unanswered. An
    // unanswered request is run again by the capture, and the span of a
    // registration prints the registration call's own value.
    assert.ok(inferred, 'a registration-call span is answered so the capture does not ask again');
    assert.strictEqual(
      inferred.type_string,
      'unknown',
      `expected no type for a registration-call span, got ${inferred.type_string}`
    );
    assert.deepStrictEqual(
      (inferred.any_provenance ?? []).map((entry) => [entry.path, entry.reason]),
      [['', 'handler_body_unread']]
    );
  });

  it('still drills into a payload-emission call span', async () => {
    // Transitional shape: the locator covers `res.json(users)` itself
    // rather than the payload subexpression. Drilling to the argument is
    // correct here because no callback is being registered.
    const jsonCall = spanOf('res.json(users)');
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-payload-span-control',
      requests: [
        {
          file_path: GAP_FIXTURE,
          line_number: jsonCall.line,
          span_start: jsonCall.start,
          span_end: jsonCall.end,
          infer_kind: 'response_body',
          alias: 'GapPayloadSpanControl',
        },
      ],
    });

    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'GapPayloadSpanControl'
    );
    assert.ok(inferred, 'expected inferred type for payload-emission span');
    // #257/#240: structural members, not the dangling bare `User[]`.
    assert.strictEqual(
      inferred.type_string,
      '{ id: number; name: string; }[]',
      `expected structural User[], got ${inferred.type_string}`
    );
  });

  it('accepts line-anchored function_return requests (file-based route shape)', async () => {
    // The orchestrator sends file-based route handlers (Next.js app router
    // etc.) with no span and no expression text — only the handler line.
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-line-anchored',
      requests: [
        {
          file_path: GAP_FIXTURE,
          line_number: LINE_RETURN_STYLE_HANDLER,
          infer_kind: 'function_return',
          alias: 'GapLineAnchoredReturn',
        },
      ],
    });

    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'GapLineAnchoredReturn'
    );
    assert.ok(
      inferred,
      `expected line-anchored function_return to resolve, got errors: ${JSON.stringify(response.errors)}`
    );
    // #257/#240: a named object return is expanded structurally, not the
    // dangling bare `User`.
    assert.strictEqual(inferred.type_string, '{ id: number; name: string; }');
  });

  it('unwraps a union of Promises per member', async () => {
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-promiseunion',
      requests: [
        {
          file_path: GAP_FIXTURE,
          line_number: LINE_LOAD_USERS,
          infer_kind: 'function_return',
          alias: 'GapPromiseUnionReturn',
        },
      ],
    });

    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'GapPromiseUnionReturn'
    );
    assert.ok(
      inferred,
      `expected inferred type for union-of-promises return, got errors: ${JSON.stringify(response.errors)}`
    );
    const got = inferred.type_string;
    assert.ok(
      got === 'CachedUsers | User[]' || got === 'User[] | CachedUsers',
      `expected per-member unwrap, got ${got}`
    );
  });

  it('does not let one invalid item poison the batch', async () => {
    // Real runs send every alias for a repo in one batch. A single bad
    // item (here: inverted span) must produce a per-item error, not
    // reject the whole request.
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-batch',
      requests: [
        {
          file_path: GAP_FIXTURE,
          line_number: LINE_RES_JSON_USERS,
          expression_text: 'users',
          expression_line: LINE_RES_JSON_USERS,
          infer_kind: 'response_body',
          alias: 'GapBatchValid',
        },
        {
          file_path: GAP_FIXTURE,
          line_number: 1,
          span_start: 50,
          span_end: 10,
          infer_kind: 'response_body',
          alias: 'GapBatchInvalid',
        },
      ],
    });

    const valid = response.inferred_types?.find((t) => t.alias === 'GapBatchValid');
    assert.ok(
      valid,
      `expected the valid item to resolve despite an invalid sibling, got ${JSON.stringify(response)}`
    );
    const invalid = response.inferred_types?.find((t) => t.alias === 'GapBatchInvalid');
    assert.strictEqual(invalid, undefined, 'invalid item must not produce a type');
    assert.ok(
      response.errors && response.errors.length > 0,
      'expected a per-item error for the invalid item'
    );
  });

  it('follows a route-registry object-literal handler to its return type (response)', async () => {
    // `{ method, path, handler: registryHealthHandler }` — the response contract
    // is the handler's RETURN type, one indirection from the registry entry.
    const entry = spanOf(
      "{ method: 'GET', path: '/registry-health', handler: registryHealthHandler }"
    );
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-registry-response',
      requests: [
        {
          // Line-only anchor — the real shape the scanner sends for a
          // named-handler registration (no span, no expression text).
          file_path: GAP_FIXTURE,
          line_number: entry.line,
          infer_kind: 'response_body',
          alias: 'GapRegistryResponse',
        },
      ],
    });
    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'GapRegistryResponse'
    );
    assert.ok(inferred, 'expected the registry handler return to resolve');
    assert.match(inferred!.type_string, /ok:\s*boolean/);
    assert.match(inferred!.type_string, /ts:\s*number/);
  });

  it('follows an inline handler body to its typed request read (request)', async () => {
    // The locator lands on the registration call; the request contract is the
    // type argument of `c.req.json<RegistryTrackReq>()` inside the handler body.
    const call = spanOf(
      "gapHono.post('/registry-track', async (c) => { const body = await c.req.json<RegistryTrackReq>(); return body; })"
    );
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'gap-registry-request',
      requests: [
        {
          // Line-only anchor — the real shape the scanner sends for the
          // request infer on an inline-handler registration.
          file_path: GAP_FIXTURE,
          line_number: call.line,
          infer_kind: 'request_body',
          alias: 'GapRegistryRequest',
        },
      ],
    });
    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'GapRegistryRequest'
    );
    assert.ok(inferred, 'expected the handler-body request read to resolve');
    assert.match(inferred!.type_string, /path:\s*string/);
    assert.match(inferred!.type_string, /userId:\s*string/);
  });
});
