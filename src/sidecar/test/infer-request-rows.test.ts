/**
 * carrick#1166: request rows a route declares only inside its handler body.
 *
 *  1. `schema.safeParse(await c.req.json())` and `schema.parse(body)`: the read
 *     itself is untyped, so the row published `unknown`. The schema that
 *     consumes the read declares what a caller sends, its INPUT (the same
 *     direction every other request anchor reads, carrick#1101).
 *  2. A validated read of a NON-body part (`c.req.valid('param')`) published
 *     the path parameters as the request body. A route whose validator binds
 *     a non-body part and reads nothing else declares no body.
 *  3. A registration with no body read, located by its whole-registration span
 *     in a file without semicolons, published `any`: the statement and its call
 *     share one span, and the tie went to the statement, whose type is `any`.
 *
 * An untyped read that nothing validates keeps its own honest `unknown`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';

const FIXTURE = path.join(FIXTURES_PATH, 'src/manual-parse-routes.ts');
const SOURCE = fs.readFileSync(FIXTURE, 'utf-8');

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; reason: string }>;
  }>;
}

function lineOf(text: string, from = 0): number {
  const at = SOURCE.indexOf(text, from);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return SOURCE.slice(0, at).split('\n').length;
}

function registration(route: string): { start: number; end: number; line: number } {
  const marker = `noteRouter.post('${route}'`;
  const start = SOURCE.indexOf(marker);
  assert.ok(start >= 0, `fixture must register ${route}`);
  let depth = 0;
  for (let i = start; i < SOURCE.length; i += 1) {
    if (SOURCE[i] === '(') depth += 1;
    if (SOURCE[i] === ')') {
      depth -= 1;
      if (depth === 0) {
        return { start, end: i + 1, line: lineOf(marker) };
      }
    }
  }
  throw new Error(`unbalanced registration for ${route}`);
}

function collapse(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

describe('carrick#1166 request rows declared in the handler body', () => {
  let client: SidecarClient;

  before(async () => {
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: FIXTURES_PATH });
  });

  after(async () => {
    await client.stop();
  });

  async function inferRequest(alias: string, locator: Record<string, unknown>) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [{ file_path: FIXTURE, infer_kind: 'request_body', alias, ...locator }],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  function textLocator(route: string, expression: string, from = 0) {
    const reg = registration(route);
    return {
      line_number: reg.line,
      expression_text: expression,
      expression_line: lineOf(expression, from === 0 ? reg.start : from),
    };
  }

  it('reads the input of the schema a body binding is parsed with', async () => {
    const inferred = await inferRequest('NotesFromBinding', textLocator('/notes', 'body'));
    assert.ok(inferred, 'the validated body is a contract');
    const text = collapse(inferred.type_string);
    assert.match(text, /title: string/, text);
    assert.match(text, /tags\?: string\[\]/, text);
  });

  it('reads the same input when the locator names the read itself', async () => {
    const fromRead = await inferRequest('NotesFromRead', textLocator('/notes', 'await c.req.json()'));
    const fromBinding = await inferRequest('NotesFromBinding2', textLocator('/notes', 'body'));
    assert.ok(fromRead && fromBinding);
    assert.strictEqual(collapse(fromRead.type_string), collapse(fromBinding.type_string));
  });

  it('reads a read handed straight to the schema', async () => {
    const inferred = await inferRequest('StrictFromRead', textLocator('/notes/strict', 'c.req.json()'));
    assert.ok(inferred);
    assert.match(collapse(inferred.type_string), /title: string/);
  });

  it('publishes no body for a validated path parameter', async () => {
    const inferred = await inferRequest(
      'ArchiveFromParam',
      textLocator('/notes/:id/archive', "c.req.valid('param')")
    );
    assert.ok(inferred, 'the abstain is reported, with its reason');
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
    assert.deepStrictEqual(
      (inferred.any_provenance ?? []).map((p) => p.reason),
      ['no_request_body']
    );
  });

  it('never publishes a registration statement as a request body', async () => {
    const reg = registration('/jobs/run');
    const inferred = await inferRequest('JobsFromSpan', {
      line_number: reg.line,
      span_start: reg.start,
      span_end: reg.end,
    });
    assert.ok(
      !inferred || collapse(inferred.type_string) !== 'any',
      `a route that reads no body must not publish any, got ${inferred?.type_string}`
    );
  });

  it('never takes a parameter for a function it is declared inside (#1162)', async () => {
    const inferred = await inferRequest('ConsumerParamBody', {
      line_number: lineOf('sendNote({'),
      expression_text: 'JSON.stringify(input)',
      expression_line: lineOf('body: JSON.stringify(input)'),
    });
    assert.ok(inferred, 'a serialised parameter is a consumer body, not a route registration');
    assert.strictEqual(collapse(inferred.type_string), '{ title: string; tags: string[]; }');
  });

  it('keeps an honest unknown for an untyped read nothing validates', async () => {
    const inferred = await inferRequest('LooseFromBinding', textLocator('/notes/loose', 'payload'));
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
  });
});
