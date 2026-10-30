/**
 * carrick#3053 (H9): a handler that returns a labelled reply construction
 * (`new Reply(body)`, `Reply.json(body)`, a subclass's inherited `json`)
 * types its reply from the service's labels. A `new` send is keyed by its
 * construct signature's declaration, a static call by the member's
 * declaration; both are matched by declaration, never by name. Nothing here
 * names a platform class: `fixture-reply` is an invented package whose class
 * is not response-shaped, so only a label can say its construction is a send.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'labelled-construct');
const PKG = 'fixture-reply';

let sequence = 0;
const check = (exported: string, claim: Record<string, unknown>) => ({
  claim_id: `c${sequence++}`,
  package: PKG,
  export: exported,
  role: 'server_framework',
  receiver: 'export',
  claim,
});
const LABELS = ['Reply', 'AppReply', 'Bare'].flatMap((exported) => [
  check(exported, { kind: 'op', op: 'send', member: 'json', on: 'export', payload: { arg: 0 } }),
  check(exported, { kind: 'op', op: 'send', member: null, on: 'export', form: 'new', payload: { arg: 0 } }),
]);

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ path: string; reason: string }>;
}
const collapse = (text: string | undefined): string => (text ?? '').replace(/\s+/g, ' ').trim();

describe('carrick#3053: labelled `new` and static reply constructions', () => {
  let root: string;
  let routesPath: string;
  let source: string;
  let client: SidecarClient;
  let id = 0;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-labelled-construct-'));
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

  const lineOf = (label: string, text: string): number => {
    const from = source.indexOf(`// ${label}\n`);
    assert.ok(from >= 0, `fixture has handler ${label}`);
    const at = source.indexOf(text, from);
    assert.ok(at >= 0, `handler ${label} contains: ${text}`);
    return source.slice(0, at).split('\n').length;
  };

  /** The row of handler `label` (on its exported function's line), located at `located`. */
  const infer = async (label: string, located: string, labels: boolean): Promise<Inferred | undefined> => {
    const alias = `${label}_${labels}_${id}`;
    const response = await client.send<{ inferred_types?: Inferred[] }>(
      {
        action: 'infer',
        request_id: `infer-${id++}`,
        requests: [
          {
            file_path: routesPath,
            line_number: lineOf(label, 'export async function'),
            infer_kind: 'response_body',
            alias,
            expression_text: located,
            expression_line: lineOf(label, located),
          },
        ],
        ...(labels ? { server_claims: { from_dir: root, checks: LABELS, type_packages: {} } } : {}),
      },
      60_000
    );
    return response.inferred_types?.find((t) => t.alias === alias);
  };
  const unaliased = (inferred: Inferred | undefined) => {
    if (!inferred) return undefined;
    const { alias: _alias, ...rest } = inferred;
    return rest;
  };
  const isUser = (got: Inferred | undefined, message: string) => {
    const text = collapse(got?.type_string);
    assert.match(text, /name: string/, `${message}: ${text}`);
    assert.doesNotMatch(text, /error|\|/, `${message}: ${text}`);
  };

  it('A: a static labelled send is a send; the error branch drops', async () => {
    isUser(await infer('A', "{ error: 'nf' }", true), 'A');
  });

  it('B: a labelled `new` send with a stringified body types the reply, located at the error branch', async () => {
    isUser(await infer('B', "{ error: 'nf' }", true), 'B');
  });

  it('C: a labelled `new` send located at its own body', async () => {
    isUser(await infer('C', 'user', true), 'C');
  });

  it('C0: without labels the same construction is no send', async () => {
    const got = await infer('B', "{ error: 'nf' }", false);
    assert.doesNotMatch(collapse(got?.type_string), /name: string/);
  });

  it('D: an unlabelled member of the same class is not a send', async () => {
    isUser(await infer('D', 'user', true), 'D');
  });

  it('E: a subclass with no constructor of its own builds through the base’s labelled one', async () => {
    isUser(await infer('E', "{ error: 'nf' }", true), 'E');
  });

  it('F: the repo’s own class with the same names reads no label', async () => {
    const located = "{ error: 'nf' }";
    assert.deepStrictEqual(unaliased(await infer('F', located, true)), unaliased(await infer('F', located, false)));
  });

  it('G: a static send reached through a re-export is matched by its declaration', async () => {
    isUser(await infer('G', "{ error: 'nf' }", true), 'G');
  });

  it('H: a `new` through a re-export is matched by its declaration', async () => {
    isUser(await infer('H', "{ error: 'nf' }", true), 'H');
  });

  it('I: a class with no constructor declaration keys no send; the labelled branch still types the reply', async () => {
    // The label on `Bare` carries a payload slot no constructor can fill, so
    // it never verifies and `new Bare()` is no send (its slot is undecided).
    isUser(await infer('I', 'user', true), 'I');
  });
});
