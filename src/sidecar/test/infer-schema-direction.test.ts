/**
 * A declared request schema publishes what a caller SENDS: the schema's input
 * type, not its parsed output (carrick#1101).
 *
 * Every request anchor that reads a validation schema used to read its output
 * (`parse` return, `_output`). A caller sends the input, and the two differ
 * wherever parsing changes the value: a defaulted key is optional to send and
 * required after parsing, and a transform changes the member's type. The
 * request check is consumer-to-producer, so publishing the output turns a
 * caller that correctly omits a defaulted key into a mismatch.
 *
 * Locked in here, across the three request paths (validator middleware, route
 * `schema.body`, and a located validated read on its own line):
 *
 *  1. the input is read from the Standard Schema member
 *     `~standard.types.input` first, then `_input`, and only a schema with
 *     neither publishes its output;
 *  2. a response bound to the same schema keeps the output;
 *  3. a member whose input is `unknown` where the output is concrete (a
 *     coercion accepts any value) publishes its OUTPUT type and is recorded as
 *     `coerced_input` in `any_provenance`, rather than abstaining. The decision
 *     is per member (carrick#1105): every other member keeps its input, so a
 *     defaulted key beside a coerced one stays optional, and the key's own
 *     optionality always comes from the input.
 *
 * The fake `schema-lib` exposes both members, and `standard-lib` exposes only
 * `~standard`, so each read order is exercised on its own.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';

const FIXTURE = path.join(FIXTURES_PATH, 'src/schema-direction-routes.ts');
const FIXTURE_SOURCE = fs.readFileSync(FIXTURE, 'utf-8');

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ path: string; kind: string; reason: string; detail?: string }>;
}

interface InferResponseShape {
  request_id: string;
  status: string;
  inferred_types?: Inferred[];
  errors?: string[];
}

/**
 * Byte span of the registration that starts at `marker`, to its matching close
 * paren. The fixture is ASCII-only, so byte offsets are character offsets.
 */
function registrationSpan(marker: string): { start: number; end: number; line: number } {
  const start = FIXTURE_SOURCE.indexOf(marker);
  assert.ok(start >= 0, `fixture must contain: ${marker}`);
  assert.strictEqual(FIXTURE_SOURCE.indexOf(marker, start + 1), -1, `${marker} must be unique`);
  let depth = 0;
  let end = -1;
  for (let i = start; i < FIXTURE_SOURCE.length; i += 1) {
    if (FIXTURE_SOURCE[i] === '(') depth += 1;
    if (FIXTURE_SOURCE[i] === ')') {
      depth -= 1;
      if (depth === 0) {
        end = i + 1;
        break;
      }
    }
  }
  assert.ok(end > start, `unbalanced registration at ${marker}`);
  return { start, end, line: FIXTURE_SOURCE.slice(0, start).split('\n').length };
}

/** 1-based line of the first occurrence of `text`. */
function lineOf(text: string): number {
  const at = FIXTURE_SOURCE.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return FIXTURE_SOURCE.slice(0, at).split('\n').length;
}

describe('schema direction: requests read the input, responses the output', () => {
  let client: SidecarClient;

  async function infer(
    alias: string,
    request: Record<string, unknown>
  ): Promise<Inferred | undefined> {
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: `schema-direction-${alias}`,
      requests: [{ file_path: FIXTURE, alias, ...request }],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  }

  async function requestBySpan(marker: string, alias: string): Promise<Inferred | undefined> {
    const span = registrationSpan(marker);
    return infer(alias, {
      infer_kind: 'request_body',
      line_number: span.line,
      span_start: span.start,
      span_end: span.end,
    });
  }

  before(async () => {
    client = new SidecarClient();
    await client.start();
    await client.send({
      action: 'init',
      request_id: 'schema-direction-init',
      repo_root: FIXTURES_PATH,
    });
  });

  after(async () => {
    await client.stop();
  });

  it('validator middleware: a defaulted key is optional in the request (registration span)', async () => {
    const inferred = await requestBySpan("router.post('/profile'", 'ProfileSpan');
    const type = inferred?.type_string;
    assert.ok(type, 'the validated body must resolve');
    assert.match(type, /displayName\s*:\s*string/, `required key missing from: ${type}`);
    assert.match(type, /theme\s*\?\s*:\s*string/, `defaulted key must be optional in: ${type}`);
    assert.match(type, /notify\s*\?\s*:\s*boolean/, `defaulted key must be optional in: ${type}`);
    assert.strictEqual(inferred?.any_provenance, undefined, 'no coercion, no provenance');
  });

  it('validator middleware: the same input when the text locator sits on the registration line', async () => {
    const inferred = await infer('ProfileText', {
      infer_kind: 'request_body',
      line_number: registrationSpan("router.post('/profile'").line,
      expression_text: "c.req.valid('json')",
      expression_line: lineOf("const profile = c.req.valid('json');"),
    });
    const type = inferred?.type_string;
    assert.ok(type, 'the validated read must resolve');
    assert.match(type, /theme\s*\?\s*:\s*string/, `defaulted key must be optional in: ${type}`);
  });

  it('a validated read located on its OWN line publishes the input too', async () => {
    // No registration starts on this line, so the declared-anchor lookup by
    // line misses and the located expression is read. Its own type is the
    // handler's parsed OUTPUT; the enclosing registration's schema says what a
    // caller sends.
    const line = lineOf("const profile = c.req.valid('json');");
    const inferred = await infer('ProfileOwnLine', {
      infer_kind: 'request_body',
      line_number: line,
      expression_text: "c.req.valid('json')",
      expression_line: line,
    });
    const type = inferred?.type_string;
    assert.ok(type, 'the validated read must resolve');
    assert.match(type, /displayName\s*:\s*string/, `required key missing from: ${type}`);
    assert.match(type, /theme\s*\?\s*:\s*string/, `defaulted key must be optional in: ${type}`);
  });

  it('a transformed member publishes the type a caller sends, not the transform result', async () => {
    const type = (await requestBySpan("router.post('/tags'", 'Tags'))?.type_string;
    assert.ok(type, 'the validated body must resolve');
    assert.match(type, /tags\s*:\s*string\s*;/, `transform input must be string in: ${type}`);
    assert.doesNotMatch(type, /string\[\]/, `transform output leaked into: ${type}`);
  });

  it('a coerced member publishes the output and says the input was coerced', async () => {
    const inferred = await requestBySpan("router.post('/page'", 'Page');
    assert.ok(inferred, 'a coerced body must still resolve, not abstain');
    const type = inferred.type_string;
    // `page` is defaulted AND coerced: optional to send (the input's key),
    // typed as what parsing produces (the output's member).
    assert.match(type, /page\s*\?\s*:\s*number\s*;/, `coerced key must read as the output, optional as the input in: ${type}`);
    assert.match(type, /label\s*:\s*string/, `plain key missing from: ${type}`);
    assert.doesNotMatch(type, /\bunknown\b/, `the unknown input must not be published: ${type}`);
    assert.deepStrictEqual(
      (inferred.any_provenance ?? []).map(({ path: p, kind, reason }) => ({ path: p, kind, reason })),
      [{ path: 'page', kind: 'unknown', reason: 'coerced_input' }],
      `coerced member must be labelled: ${JSON.stringify(inferred.any_provenance)}`
    );
  });

  it('a coerced member beside a defaulted one: each member keeps its own direction', async () => {
    const inferred = await requestBySpan("router.post('/mixed'", 'Mixed');
    assert.ok(inferred, 'a mixed body must resolve');
    const type = inferred.type_string;
    assert.match(type, /page\s*:\s*number\s*;/, `coerced key must read as the output in: ${type}`);
    assert.match(type, /theme\s*\?\s*:\s*string\s*;/, `defaulted key must stay optional beside a coerced one in: ${type}`);
    assert.match(type, /label\s*:\s*string\s*;/, `plain key missing from: ${type}`);
    assert.doesNotMatch(type, /\bunknown\b/, `the unknown input must not be published: ${type}`);
    assert.deepStrictEqual(
      (inferred.any_provenance ?? []).map(({ path: p, kind, reason }) => ({ path: p, kind, reason })),
      [{ path: 'page', kind: 'unknown', reason: 'coerced_input' }],
      `only the coerced member is labelled: ${JSON.stringify(inferred.any_provenance)}`
    );
  });

  it('coerced and defaulted members inside nested objects and arrays keep their own directions', async () => {
    const inferred = await requestBySpan("router.post('/nested'", 'Nested');
    assert.ok(inferred, 'a nested body must resolve');
    const type = inferred.type_string;
    assert.match(
      type,
      /filter\s*:\s*\{\s*limit\s*:\s*number\s*;\s*sort\s*\?\s*:\s*string\s*;\s*\}/,
      `nested object must mix output and input members in: ${type}`
    );
    assert.match(type, /ids\s*:\s*number\[\]/, `coerced array element must read as the output in: ${type}`);
    assert.match(
      type,
      /lines\s*:\s*\{\s*qty\s*:\s*number\s*;\s*note\s*\?\s*:\s*string\s*;\s*\}\[\]/,
      `array of objects must mix output and input members in: ${type}`
    );
    assert.match(type, /label\s*\?\s*:\s*string\s*;/, `defaulted top-level key must stay optional in: ${type}`);
    assert.match(type, /offset\s*\?\s*:\s*number\s*;/, `optional coerced key must read as the output in: ${type}`);
    assert.doesNotMatch(type, /\bunknown\b/, `no unknown input may be published: ${type}`);
    assert.deepStrictEqual(
      (inferred.any_provenance ?? []).map(({ path: p, kind, reason }) => ({ path: p, kind, reason })),
      [
        { path: 'filter.limit', kind: 'unknown', reason: 'coerced_input' },
        { path: 'ids<0>', kind: 'unknown', reason: 'coerced_input' },
        { path: 'lines<0>.qty', kind: 'unknown', reason: 'coerced_input' },
        { path: 'offset', kind: 'unknown', reason: 'coerced_input' },
      ],
      `each coerced position is labelled: ${JSON.stringify(inferred.any_provenance)}`
    );
  });

  it('a coerced position the printer cannot reach falls back to the whole parsed output, labelled', async () => {
    // A tuple is printed by name, so its coerced element cannot be substituted
    // member by member. Publishing the input there would publish `unknown`.
    const inferred = await requestBySpan("router.post('/pair'", 'Pair');
    assert.ok(inferred, 'a body with an unreachable coercion must still resolve');
    const type = inferred.type_string;
    assert.match(type, /point\s*:\s*\[number,\s*string\]/, `tuple must read as the output in: ${type}`);
    assert.match(type, /unit\s*:\s*string/, `whole-output fallback expected in: ${type}`);
    assert.doesNotMatch(type, /\bunknown\b/, `the unknown input must not be published: ${type}`);
    assert.deepStrictEqual(
      (inferred.any_provenance ?? []).map(({ path: p, kind, reason }) => ({ path: p, kind, reason })),
      [{ path: 'point.0', kind: 'unknown', reason: 'coerced_input' }],
      `the coerced element is labelled: ${JSON.stringify(inferred.any_provenance)}`
    );
  });

  it('a schema exposing only Standard Schema publishes its input', async () => {
    const type = (await requestBySpan("router.post('/standard'", 'Standard'))?.type_string;
    assert.ok(type, 'a Standard Schema body must resolve');
    assert.match(type, /title\s*:\s*string/, `required key missing from: ${type}`);
    assert.match(type, /priority\s*\?\s*:\s*number/, `defaulted key must be optional in: ${type}`);
  });

  it('a schema exposing only parse still resolves, to its output', async () => {
    const type = (await requestBySpan("router.post('/parse-only'", 'ParseOnly'))?.type_string;
    assert.ok(type, 'a parse-only body must resolve');
    assert.match(type, /ticket\s*:\s*string/, `output missing from: ${type}`);
  });

  it('route schema object: the body reads the input and the 200 response reads the output', async () => {
    const marker = "server.post(\n    '/profile/schema'";
    const request = (await requestBySpan(marker, 'ProfileSchemaReq'))?.type_string;
    assert.ok(request, 'the declared body must resolve');
    assert.match(request, /theme\s*\?\s*:\s*string/, `request defaulted key must be optional in: ${request}`);

    const span = registrationSpan(marker);
    const response = (
      await infer('ProfileSchemaRes', {
        infer_kind: 'function_return',
        line_number: span.line,
        span_start: span.start,
        span_end: span.end,
      })
    )?.type_string;
    assert.ok(response, 'the declared response must resolve');
    assert.match(response, /theme\s*:\s*string/, `response defaulted key must stay required in: ${response}`);
    assert.doesNotMatch(response, /theme\s*\?/, `response must not read the input: ${response}`);
  });

  it('route schema object: a Standard-Schema-only response reads ~standard.types.output', async () => {
    const marker = "server.post(\n    '/standard/schema'";
    const span = registrationSpan(marker);
    const response = (
      await infer('StandardSchemaRes', {
        infer_kind: 'function_return',
        line_number: span.line,
        span_start: span.start,
        span_end: span.end,
      })
    )?.type_string;
    assert.ok(response, 'the declared response must resolve');
    assert.match(response, /priority\s*:\s*number/, `output key must be required in: ${response}`);
    assert.doesNotMatch(response, /priority\s*\?/, `response must not read the input: ${response}`);
  });
});
