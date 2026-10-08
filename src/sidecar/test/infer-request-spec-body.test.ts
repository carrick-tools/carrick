/**
 * carrick#1841, request half: the body of a request call that states its URL
 * on its one object argument.
 *
 * A generated client issues every operation as
 * `client.post<…>({ url: '/items', ...options })`. The method's own parameter
 * types the body as `unknown`, and the call has one argument, so the readers
 * that follow a body or config parameter of the method's signature find
 * nothing. The body the operation sends is the `body` member of the object it
 * hands the method: written there, or carried in by a spread whose type says
 * what it is. The scanner locates such a row at the call.
 *
 * The fixture is a hand-written client of that shape with neutral names.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'status-table-client');

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; kind: string; reason: string }>;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1841: the body of a request call stated on its one object argument', () => {
  let client: SidecarClient;
  let repoDir: string;
  let source: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1841-request-'));
    fs.cpSync(FIXTURE, repoDir, { recursive: true });
    source = fs.readFileSync(path.join(repoDir, 'src', 'sdk.gen.ts'), 'utf8');
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The request body at the call starting with `callStart`, located by the call's span, as the scanner sends it. */
  async function requestBodyAt(callStart: string) {
    const start = source.indexOf(callStart);
    assert.ok(start >= 0, `the fixture holds ${callStart}`);
    const end = source.indexOf('})', start) + 2;
    const line = source.slice(0, start).split('\n').length;
    const alias = `Endpoint_${line}_Request`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: path.join(repoDir, 'src', 'sdk.gen.ts'),
          line_number: line,
          infer_kind: 'request_body',
          alias,
          span_start: start,
          span_end: end,
        },
      ],
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, 'the row must be answered');
    return inferred;
  }

  it('reads the body a spread carries onto the request object', async () => {
    const inferred = await requestBodyAt('client.post<CreateItemResponses');
    assert.strictEqual(collapse(inferred.type_string), '{ name: string; }');
  });

  it('reads the body written on the request object', async () => {
    const inferred = await requestBodyAt('client.patch<GetItemResponses');
    assert.strictEqual(collapse(inferred.type_string), '{ name: string; }');
  });

  it('a data type that states no body sends none, as a decided abstain', async () => {
    const inferred = await requestBodyAt('client.delete<DeleteItemResponses, DeleteItemErrors>({ url: \'/items/{id}\', ...options })');
    assert.strictEqual(inferred.type_string.trim(), 'unknown', inferred.type_string);
    assert.deepStrictEqual(
      (inferred.any_provenance ?? []).map((p) => [p.path, p.reason]),
      [['', 'no_request_body']],
      'the abstain must be decided, or the capture re-reads the request object'
    );
  });
});
