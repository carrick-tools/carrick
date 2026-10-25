/**
 * A payload locator that elides members with `...` names the whole payload.
 *
 * The model writes a long request payload as its opening and a `...`:
 * `body: { customerId: accountId, ... }`. That text is not in the source, and
 * not valid syntax, so the locator has no parse to read. The reverse rule (a
 * node whose text the locator contains) then bound the one member the text
 * quotes in full, `customerId: accountId`, which covers over half of the
 * text: the request was typed as that member's value (`string`) and a right
 * body read incompatible.
 *
 * An elision stands for the members it hides. The text before it is where the
 * source opens, so the locator binds the node whose text opens alike, or
 * nothing. A real spread (`...rest`) is source text and is untouched.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SOURCE_TS = `declare const jobs: { start(input: { path: string; body: object }): void };
declare const accountId: string;
declare const containerId: string;
declare const retries: number;
declare const rest: { note: string };

export function startPlain() {
  jobs.start({ path: '/jobs', body: { customerId: accountId, containerId: containerId, retries: retries } });
}

export function startWrapped() {
  jobs.start({
    path: '/jobs',
    body: { customerId: accountId, containerId: containerId, retries: retries },
  });
}

export function startSpread() {
  jobs.start({ path: '/jobs', body: { customerId: accountId, ...rest } });
}
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

function lineOf(marker: string): number {
  const index = SOURCE_TS.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
  return index + 1;
}

const ONE_LINE = 'body: { customerId: accountId, containerId: containerId, retries: retries } });';
const WHOLE = '{ customerId: string; containerId: string; retries: number; }';

describe('an elided payload locator never binds a fragment of the payload', () => {
  let client: SidecarClient;
  let repoDir: string;
  let sourcePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-elided-'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    sourcePath = path.join(repoDir, 'src', 'source.ts');
    fs.writeFileSync(sourcePath, SOURCE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (
    alias: string,
    expression: string,
    marker: string
  ): Promise<string | undefined> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: sourcePath,
          line_number: lineOf(marker),
          expression_text: expression,
          expression_line: lineOf(marker),
          infer_kind: 'request_body',
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    return inferred ? collapse(inferred.type_string) : undefined;
  };

  it('reads the whole payload for a property locator that ends in an elision', async () => {
    const text = await infer('Property', 'body: { customerId: accountId, ... }', ONE_LINE);
    assert.strictEqual(text, WHOLE);
  });

  it('reads the whole payload for an object locator that ends in an elision', async () => {
    const text = await infer('Object', '{ customerId: accountId, containerId: containerId, ... }', ONE_LINE);
    assert.strictEqual(text, WHOLE);
  });

  it('reads the whole payload when the object sits on its own line in a wrapped call', async () => {
    const text = await infer('Wrapped', 'body: { customerId: accountId, ... }', "    body: { customerId: accountId, containerId");
    assert.strictEqual(text, WHOLE);
  });

  it('binds no fragment when the opening is not in the source', async () => {
    const text = await infer('Missing', 'body: { accountRef: accountId, ... }', ONE_LINE);
    assert.strictEqual(text, undefined);
  });

  it('keeps a real spread as source text', async () => {
    const text = await infer('Spread', 'body: { customerId: accountId, ...rest }', '...rest }');
    assert.strictEqual(text, '{ note: string; customerId: string; }');
  });
});
