/**
 * The request contract of a route whose body is declared by a VALIDATOR
 * MIDDLEWARE, and the guard that stops a callable context member being
 * published as a request contract (carrick#964).
 *
 * A context-object framework (Hono is the one this was found on) gives the
 * handler a single context that both reads the request and sends the response.
 * That context has a `body` MEMBER — the response sender — so the
 * handler-parameter anchor, which reads `body` off each parameter's type,
 * answered the sender's callable type for every route in the service and
 * published it as an explicit request contract.
 *
 * Two things are locked in here:
 *
 *  1. a type with call signatures is never a payload, on any anchor or on the
 *     located expression itself; and
 *  2. when a validator middleware on the registration binds a schema to the
 *     request body, the schema's parsed output IS the request contract,
 *     required and optional members intact.
 *
 * A validator bound to a non-body part declares no request body, and a route
 * that declares nothing stays unresolved — never a framework internal.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';

const FIXTURE = path.join(FIXTURES_PATH, 'src/validator-middleware-routes.ts');
const FIXTURE_SOURCE = fs.readFileSync(FIXTURE, 'utf-8');

interface InferResponseShape {
  request_id: string;
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    infer_kind: string;
    is_explicit: boolean;
  }>;
  errors?: string[];
}

/**
 * Byte span of one route registration, from `router.<method>('<route>'` to the
 * matching close paren — the whole-registration span the scanner falls back to
 * when the model reported no payload expression. The fixture is ASCII-only, so
 * byte offsets are character offsets.
 */
function registrationSpan(
  method: 'post' | 'get',
  route: string
): { start: number; end: number; line: number } {
  const marker = `router.${method}('${route}'`;
  const start = FIXTURE_SOURCE.indexOf(marker);
  assert.ok(start >= 0, `fixture must register: ${method} ${route}`);
  assert.strictEqual(
    FIXTURE_SOURCE.indexOf(marker, start + 1),
    -1,
    `fixture must register ${route} exactly once`
  );

  let depth = 0;
  let end = -1;
  for (let i = start; i < FIXTURE_SOURCE.length; i += 1) {
    const char = FIXTURE_SOURCE[i];
    if (char === '(') depth += 1;
    if (char === ')') {
      depth -= 1;
      if (depth === 0) {
        end = i + 1;
        break;
      }
    }
  }
  assert.ok(end > start, `unbalanced registration for ${route}`);
  return {
    start,
    end,
    line: FIXTURE_SOURCE.slice(0, start).split('\n').length,
  };
}

/** 1-based line the first occurrence of `text` starts on. */
function lineOf(text: string): number {
  const at = FIXTURE_SOURCE.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return FIXTURE_SOURCE.slice(0, at).split('\n').length;
}

async function inferRequest(
  client: SidecarClient,
  alias: string,
  locator: {
    line_number: number;
    span_start?: number;
    span_end?: number;
    expression_text?: string;
    expression_line?: number;
  }
): Promise<string | undefined> {
  const response = await client.send<InferResponseShape>({
    action: 'infer',
    request_id: `validator-middleware-${alias}`,
    requests: [
      {
        file_path: FIXTURE,
        infer_kind: 'request_body',
        alias,
        ...locator,
      },
    ],
  });
  return response.inferred_types?.find((t) => t.alias === alias)?.type_string;
}

describe('validator-middleware request contracts', () => {
  let client: SidecarClient;

  before(async () => {
    client = new SidecarClient();
    await client.start();
    await client.send({
      action: 'init',
      request_id: 'validator-middleware-init',
      repo_root: FIXTURES_PATH,
    });
  });

  after(async () => {
    await client.stop();
  });

  it('reads the validated body schema from a whole-registration span', async () => {
    const span = registrationSpan('post', '/search');
    const type = await inferRequest(client, 'SearchRequestSpan', {
      line_number: span.line,
      span_start: span.start,
      span_end: span.end,
    });

    assert.ok(type, 'the validated body must resolve');
    assert.match(
      type,
      /term\s*:\s*string/,
      `required member missing from: ${type}`
    );
    assert.match(
      type,
      /regions\s*\?\s*:\s*string\[\]/,
      `optional member missing (or not optional) in: ${type}`
    );
    assert.doesNotMatch(
      type,
      /BodySender/,
      'the context response sender must never be published as a request contract'
    );
  });

  it('reads the same contract when the locator lands on the validated read', async () => {
    // The live shape: `line_number` is the registration's (the endpoint's own
    // line), while the text locator the model reported points inside the
    // handler. The registration's declared contract is consulted first, so this
    // exercises the declared-anchor path, not the located expression.
    const expression = "c.req.valid('json')";
    const type = await inferRequest(client, 'SearchRequestText', {
      line_number: registrationSpan('post', '/search').line,
      expression_text: expression,
      expression_line: lineOf(expression),
    });

    assert.ok(type, 'the validated read must resolve');
    assert.match(type, /term\s*:\s*string/, `required member missing from: ${type}`);
    assert.match(
      type,
      /regions\s*\?\s*:\s*string\[\]/,
      `optional member missing (or not optional) in: ${type}`
    );
    assert.doesNotMatch(type, /BodySender/);
  });

  it('declares no request body when the validator binds a non-body part', async () => {
    const span = registrationSpan('post', '/reports');
    const type = await inferRequest(client, 'ReportsRequest', {
      line_number: span.line,
      span_start: span.start,
      span_end: span.end,
    });

    assert.strictEqual(
      type,
      undefined,
      `a query validator declares no request body, got: ${type}`
    );
  });

  it('leaves a route that declares nothing unresolved', async () => {
    const span = registrationSpan('get', '/health');
    const type = await inferRequest(client, 'HealthRequestSpan', {
      line_number: span.line,
      span_start: span.start,
      span_end: span.end,
    });

    assert.strictEqual(
      type,
      undefined,
      `a route with no declared request body must stay unresolved, got: ${type}`
    );
  });

  it('never publishes the context body sender a locator landed on', async () => {
    const expression = 'c.body';
    const type = await inferRequest(client, 'HealthRequestText', {
      line_number: lineOf('return c.body(null, 204);'),
      expression_text: expression,
      expression_line: lineOf('return c.body(null, 204);'),
    });

    assert.strictEqual(
      type,
      undefined,
      `a callable context member is not a payload, got: ${type}`
    );
  });
});
