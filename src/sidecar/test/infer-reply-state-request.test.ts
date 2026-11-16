/**
 * carrick#4690: a producer request read that is a member of the server's
 * reply object (`res.locals`) is state the server's own code keeps there, not
 * what a caller sends. It answers a decided abstain, root `unknown` with the
 * reason `reply_state`, never the reply's whole locals type. The reply object
 * is recognised by the server labels (a member of its type is a labelled
 * send), never by a name; a member the labels name as the body stays a body.
 *
 * The fixture (`fixtures/reply-locals`) is an invented server library.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'reply-locals');
const PKG = 'reply-desk';

let sequence = 0;
const check = (exported: string, receiver: string, claim: Record<string, unknown>) => ({
  claim_id: `c${sequence++}`,
  package: PKG,
  export: exported,
  role: 'server_framework',
  receiver,
  claim,
});
const send = (exported: string, member: string) =>
  check(exported, 'export', { kind: 'op', op: 'send', member, on: 'export', payload: { arg: 0 } });
const receive = (member: string, method: string) =>
  check('default', 'instance:()', {
    kind: 'op',
    op: 'receive',
    member,
    of: 'instance:()',
    name: { arg: 0 },
    handler: { arg: 1 },
    method,
    request: { param: 0, body: ['body'] },
  });

const LABELS = [
  check('default', 'export', { kind: 'make', form: 'call', member: null }),
  receive('post', 'POST'),
  receive('put', 'PUT'),
  send('reply', 'json'),
  send('reply', 'send'),
  send('ctx', 'json'),
];

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ path: string; reason: string }>;
}

const collapse = (text: string | undefined): string => (text ?? '').replace(/\s+/g, ' ').trim();

describe('carrick#4690: a request read off the reply object is reply state', () => {
  let root: string;
  let source: string;
  let file: string;
  let client: SidecarClient;
  let id = 0;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-reply-state-'));
    fs.cpSync(FIXTURE, root, { recursive: true });
    file = path.join(root, 'src', 'holds.ts');
    source = fs.readFileSync(file, 'utf8');
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: root });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  const lineOf = (text: string, from = 0): number => {
    const at = source.indexOf(text, from);
    assert.ok(at >= 0, `fixture contains: ${text}`);
    return source.slice(0, at).split('\n').length;
  };

  /** The request row of handler `label`, located at `located` on the handler's body line. */
  const infer = async (label: string, route: string, located: string, labels: boolean): Promise<Inferred | undefined> => {
    const from = source.indexOf(`// ${label}\n`);
    const alias = `${label}_${labels}_${id}`;
    const response = await client.send<{ inferred_types?: Inferred[] }>(
      {
        action: 'infer',
        request_id: `infer-${id++}`,
        requests: [
          {
            file_path: file,
            line_number: lineOf(`app.`, source.indexOf(route, from) - 12),
            infer_kind: 'request_body',
            alias,
            expression_text: located,
            expression_line: lineOf(located, from),
          },
        ],
        ...(labels ? { server_claims: { from_dir: root, checks: LABELS, type_packages: {} } } : {}),
      },
      60_000
    );
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  const reason = (inferred: Inferred | undefined) =>
    inferred?.any_provenance?.find((p) => p.path === '')?.reason;

  it('a labelled reply type: `res.locals` is a decided abstain, never the locals type', async () => {
    const got = await infer('release', "'/release'", 'res.locals', true);
    assert.strictEqual(collapse(got?.type_string), 'unknown', collapse(got?.type_string));
    assert.strictEqual(reason(got), 'reply_state');
  });

  it('no labels: the read is unchanged (the whole locals; a filed gap)', async () => {
    const got = await infer('release', "'/release'", 'res.locals', false);
    assert.match(collapse(got?.type_string), /parsedBody/, collapse(got?.type_string));
    assert.notStrictEqual(reason(got), 'reply_state');
  });

  it('`req.body` keeps its reading with labels', async () => {
    const got = await infer('take', "'/take'", 'req.body', true);
    assert.match(collapse(got?.type_string), /holder: string/, collapse(got?.type_string));
    assert.notStrictEqual(reason(got), 'reply_state');
  });

  it('`c.body` read in a helper of a route registered with an unstated context keeps its reading', async () => {
    const got = await infer('helper', "'/helper'", 'c.body', true);
    assert.match(collapse(got?.type_string), /holder: string/, collapse(got?.type_string));
    assert.notStrictEqual(reason(got), 'reply_state');
  });

  it('`c.body` on a context holding a send and the body keeps its reading (labelled body member)', async () => {
    const got = await infer('context', "'/context'", 'c.body', true);
    assert.match(collapse(got?.type_string), /holder: string/, collapse(got?.type_string));
    assert.notStrictEqual(reason(got), 'reply_state');
  });
});
