/**
 * Route-contract anchors for schema-first services (carrick#528).
 *
 * A route registered as
 *
 *   server.post(path, { schema: { body: $ref('X'), response: { 200: $ref('Y') } } },
 *               async (request: TypedRequest, reply) => controller(request, reply))
 *
 * writes its contract down in two places, and evaluates NEITHER: the handler's
 * parameter annotation and the registration's schema object. The pre-existing
 * request path followed the handler and looked for a typed request READ in its
 * body; a forwarding arrow has none, so it returned null and the endpoint's
 * request type stayed `any`. The response path followed the handler's RETURN
 * type, which for a `reply.send(...)` forwarder is framework machinery, so the
 * response stayed `any` too.
 *
 * These lock the two anchors in, in order, and lock in that the pre-existing
 * typed-read anchor still wins on a route that declares neither.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';

const FIXTURE = path.join(FIXTURES_PATH, 'src/route-schema-anchors.ts');
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
 * Byte span of one route registration, from `server.post(\n    '<route>'` to the
 * matching close paren — the same whole-registration span the scanner sends
 * from its SWC candidate. The fixture is ASCII-only, so byte == char offsets.
 */
function registrationSpan(route: string): {
  start: number;
  end: number;
  line: number;
} {
  const marker = `server.post(\n    '${route}',`;
  const start = FIXTURE_SOURCE.indexOf(marker);
  assert.ok(start >= 0, `fixture must register: ${route}`);
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

async function infer(
  client: SidecarClient,
  route: string,
  kind: 'request_body' | 'response_body' | 'function_return',
  alias: string
): Promise<string | undefined> {
  const span = registrationSpan(route);
  const response = await client.send<InferResponseShape>({
    action: 'infer',
    request_id: `route-schema-${alias}`,
    requests: [
      {
        file_path: FIXTURE,
        line_number: span.line,
        span_start: span.start,
        span_end: span.end,
        infer_kind: kind,
        alias,
      },
    ],
  });
  return response.inferred_types?.find((t) => t.alias === alias)?.type_string;
}

describe('schema-first route contract anchors', () => {
  let client: SidecarClient;

  before(async () => {
    client = new SidecarClient();
    await client.start();
    await client.send({
      action: 'init',
      request_id: 'route-schema-init',
      repo_root: FIXTURES_PATH,
    });
  });

  after(async () => {
    await client.stop();
  });

  it('anchor (a): a POST whose handler annotates its request parameter resolves the request body from that annotation', async () => {
    const type = await infer(client, '/widgets', 'request_body', 'WidgetsReq');

    assert.ok(type, 'expected a resolved request type, got none');
    assert.notStrictEqual(type, 'any', 'request must not resolve to any');
    assert.match(type, /name: string/);
    assert.match(type, /sizeCm: number/);
  });

  it('anchor (b): the same POST resolves its response from the declared 200 schema', async () => {
    const type = await infer(client, '/widgets', 'response_body', 'WidgetsRes');

    assert.ok(type, 'expected a resolved response type, got none');
    assert.notStrictEqual(type, 'any', 'response must not resolve to any');
    assert.match(type, /id: string/);
    assert.match(type, /sizeCm: number/);
    // The declared response schema, not the request schema.
    assert.doesNotMatch(type, /tags/);
  });

  it('anchor (b): the declared response also answers a function_return locator on the registration', async () => {
    // The scanner sends `function_return` for a route whose handler returns its
    // payload. Its containing-function walk resolves the function REGISTERING
    // the route, so without the anchor this reports that function's return
    // (`void` here), not the route's contract.
    const type = await infer(
      client,
      '/widgets',
      'function_return',
      'WidgetsRet'
    );

    assert.ok(type, 'expected a resolved response type, got none');
    assert.notStrictEqual(type, 'void', 'must not report the registering function return');
    assert.match(type, /id: string/);
    assert.match(type, /sizeCm: number/);
  });

  it('anchor (b): a POST with no typed request parameter falls through to the declared body schema', async () => {
    const type = await infer(
      client,
      '/widgets/import',
      'request_body',
      'ImportReq'
    );

    assert.ok(type, 'expected a resolved request type, got none');
    assert.notStrictEqual(type, 'any', 'request must not resolve to any');
    assert.match(type, /sourceUrl: string/);
    assert.match(type, /overwrite: boolean/);
  });

  it('a route declaring no schema and no typed parameter still resolves from the typed request read in its handler', async () => {
    const type = await infer(
      client,
      '/widgets/legacy',
      'request_body',
      'LegacyReq'
    );

    assert.ok(type, 'expected a resolved request type, got none');
    assert.match(type, /legacyId: number/);
    assert.match(type, /label: string/);
  });
});
