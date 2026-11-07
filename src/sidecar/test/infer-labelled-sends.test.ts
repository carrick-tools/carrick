/**
 * carrick#2472 C2: the response-site walk asks the service's server-framework
 * labels which calls are body sends before it falls back to its structural
 * reading. A labelled `send` member is a send wherever it sits in the
 * handler; an unlabelled member of a reply object that has labelled sends is
 * a setter (a header, a cookie, a status), never a body. Members are matched
 * by declaration, never by name.
 *
 * The fixture (`fixtures/labelled-sends`) is an invented server library whose
 * reply object returns itself from sends and setters alike. Each row is
 * located at an error send, so the walk publishes the handler's success
 * sends instead (carrick#2427).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'labelled-sends');
const PKG = 'reply-kit';

let sequence = 0;
const check = (exported: string, receiver: string, claim: Record<string, unknown>) => ({
  claim_id: `c${sequence++}`,
  package: PKG,
  export: exported,
  role: 'server_framework',
  receiver,
  claim,
});
const send = (member: string) =>
  check('reply', 'export', { kind: 'op', op: 'send', member, on: 'export', payload: { arg: 0 } });

/** The labels as the store answers them: `json`, `send`, `end` and `write` are sends. */
const LABELS = [
  check('default', 'export', { kind: 'make', form: 'call', member: null }),
  ...['get', 'post'].map((member) =>
    check('default', 'instance:()', {
      kind: 'op',
      op: 'receive',
      member,
      of: 'instance:()',
      name: { arg: 0 },
      handler: { arg: 1 },
      method: member.toUpperCase(),
      request: { param: 0, body: ['body'] },
    })
  ),
  send('json'),
  send('send'),
  send('end'),
  send('write'),
];

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ path: string; reason: string }>;
}

const collapse = (text: string | undefined): string => (text ?? '').replace(/\s+/g, ' ').trim();

describe('carrick#2472 C2: labelled sends in the response-site walk', () => {
  let root: string;
  let routesPath: string;
  let source: string;
  let client: SidecarClient;
  let id = 0;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-labelled-sends-'));
    fs.cpSync(FIXTURE, root, { recursive: true });
    routesPath = path.join(root, 'src', 'routes.ts');
    source = fs.readFileSync(routesPath, 'utf8');
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: root });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  /** One-based line of `text`, searched from the `// <label>` comment. */
  const lineOf = (label: string, text: string): number => {
    const from = source.indexOf(`// ${label}\n`);
    assert.ok(from >= 0, `fixture has handler ${label}`);
    const at = source.indexOf(text, from);
    assert.ok(at >= 0, `handler ${label} contains: ${text}`);
    return source.slice(0, at).split('\n').length;
  };

  /** The response row of handler `label`, located at `located` on the line holding `at`. */
  const infer = async (
    label: string,
    located: string,
    labels: boolean,
    at: string = located
  ): Promise<Inferred | undefined> => {
    const alias = `${label}_${labels}_${id}`;
    const response = await client.send<{ inferred_types?: Inferred[] }>(
      {
        action: 'infer',
        request_id: `infer-${id++}`,
        requests: [
          {
            file_path: routesPath,
            line_number: lineOf(label, 'app.'),
            infer_kind: 'response_body',
            alias,
            expression_text: located,
            expression_line: lineOf(label, at),
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
  const unaliased = (inferred: Inferred | undefined) => {
    if (!inferred) return undefined;
    const { alias: _alias, ...rest } = inferred;
    return rest;
  };

  it('A: a header setter before the error branch leaves the last success send the body when it is labelled', async () => {
    const unlabelled = await infer('A', "{ error: 'bad', code: 1 }", false);
    assert.strictEqual(reason(unlabelled), 'no_success_payload', 'without labels the statement send does not end the path');
    const got = collapse((await infer('A', "{ error: 'bad', code: 1 }", true))?.type_string);
    assert.match(got, /total: number/, got);
    assert.doesNotMatch(got, /error/, got);
  });

  it('B: a success send in the else branch of an if is the body when it is labelled', async () => {
    const unlabelled = await infer('B', 'failure', false, 'json(failure)');
    assert.strictEqual(reason(unlabelled), 'no_success_payload', 'without labels the statement send does not end the path');
    const got = collapse((await infer('B', 'failure', true, 'json(failure)'))?.type_string);
    assert.match(got, /title: string/, got);
    assert.doesNotMatch(got, /error/, got);
  });

  it('C: a success send in a switch case is the body when it is labelled', async () => {
    const unlabelled = await infer('C', '{ missing: true }', false);
    assert.strictEqual(reason(unlabelled), 'no_success_payload', 'without labels a case send does not end the path');
    const got = collapse((await infer('C', '{ missing: true }', true))?.type_string);
    assert.match(got, /title: string/, got);
    assert.doesNotMatch(got, /missing/, got);
  });

  it('D: a trailing cookie setter is not the body; the labelled send before it is', async () => {
    const unlabelled = await infer('D', "{ error: 'no session', code: 2 }", false);
    assert.strictEqual(reason(unlabelled), 'no_success_payload', 'without labels no statement send is read as the body');
    const got = collapse((await infer('D', "{ error: 'no session', code: 2 }", true))?.type_string);
    assert.match(got, /total: number/, got);
    assert.doesNotMatch(got, /\|/, `the cookie value joins no union: ${got}`);
  });

  it('E: the repo’s own reply type with the same member names reads no label', async () => {
    const located = "{ error: 'no session', code: 2 }";
    assert.deepStrictEqual(unaliased(await infer('E', located, true)), unaliased(await infer('E', located, false)));
  });

  it('G: a labelled chunk write joins the body set (the label reads `write` as a send)', async () => {
    // What the walk publishes when the store labels a chunk write as a send:
    // the chunk type. A store that labels `write` so states a defect in the
    // label, not in this walk (carrick#2472 C2).
    const got = collapse((await infer('G', '{ gone: true }', true))?.type_string);
    assert.match(got, /seq: number/, got);
  });

  it('H: a labelled send with no payload argument sends no body', async () => {
    for (const labels of [false, true]) {
      const inferred = await infer('H', "{ error: 'bad', code: 3 }", labels);
      assert.strictEqual(reason(inferred), 'no_success_payload', `labels=${labels}`);
    }
  });
});
