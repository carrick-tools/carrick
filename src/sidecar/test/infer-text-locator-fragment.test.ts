/**
 * carrick#2056: a text locator whose text differs from the source was
 * answered by a node inside what the text names.
 *
 * Text matching tries the exact text, then a node containing it, then a node
 * the text contains. That last, reverse rule exists for drift AROUND the true
 * node (a `return` or an `await` the source does not have there, #335). Where
 * the true node's own text drifted (other quotes, a dropped statement), it
 * bound a fragment of it instead:
 *
 *  - a call written with other quotes was answered by the `new
 *    URLSearchParams(…)` inside its URL argument, so a consumer was typed as
 *    the query string it builds;
 *  - a returned object whose nested callback the text paraphrased was answered
 *    by the object that callback returns, so a route published one item of an
 *    inner list.
 *
 * Three rules, none keyed on a name:
 *
 *  - a node the locator's own parse places strictly inside the expression it
 *    names is never bound by the reverse rule;
 *  - a string literal reads the same in any quotes, on both sides;
 *  - a call-result locator that finds no call publishes nothing, rather than
 *    whatever other node its text overlaps.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SOURCE_TS = `type Row = { id: string; label: string };
type Hits = { hits: string[] };

export async function concatenated(term: string) {
  const res = await fetch(\`/api/search?\` + new URLSearchParams({ q: term }));
  const found = (await res.json()) as Hits;
  return found;
}

export async function interpolated(term: string) {
  const res = await fetch(\`/api/lookup?\${new URLSearchParams({ q: term })}\`);
  const found = (await res.json()) as Hits;
  return found;
}

export async function awaited() {
  const res = await fetch(\`/api/items\`);
  const found = (await res.json()) as Hits;
  return found;
}

declare const app: { get(path: string, handler: (query: { extra: number }) => unknown): void };
declare const rows: Row[];

app.get('/summary', ({ extra }) => {
  return {
    total: extra,
    entries: rows.map((row) => {
      const label = row.label.trim()
      return { id: row.id, label, size: label.length }
    }),
  }
});
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

const HITS = '{ hits: string[]; }';

describe('carrick#2056: a text locator never binds a fragment of what it names', () => {
  let client: SidecarClient;
  let repoDir: string;
  let sourcePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2056-'));
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
    kind: 'call_result' | 'response_body',
    expression: string,
    at: string,
    row: string = at
  ): Promise<string | undefined> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: sourcePath,
          line_number: lineOf(row),
          expression_text: expression,
          expression_line: lineOf(at),
          infer_kind: kind,
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    return inferred ? collapse(inferred.type_string) : undefined;
  };

  it('finds a call written with other quotes, and reads its body', async () => {
    const text = await infer(
      'Concatenated',
      'call_result',
      "fetch('/api/search?' + new URLSearchParams({ q: term }))",
      'await fetch(`/api/search?`'
    );
    assert.strictEqual(text, HITS);
  });

  it('publishes nothing for a call it cannot find, never a node inside its URL', async () => {
    const text = await infer(
      'Interpolated',
      'call_result',
      "fetch('/api/lookup?' + new URLSearchParams({ q: term }))",
      'await fetch(`/api/lookup?'
    );
    assert.strictEqual(text, undefined);
  });

  it('publishes nothing for a call-result locator that names no call', async () => {
    // `found` is in no call: the text names a binding, and the call's result
    // is read off the call or not at all.
    const text = await infer('Named', 'call_result', 'found', 'return found;');
    assert.strictEqual(text, undefined);
  });

  // The row sits on the route's registration, as a route row does; the
  // model's locator is on the return.
  it('publishes the object a paraphrased locator names, not the object its callback returns', async () => {
    const text = await infer(
      'Summarised',
      'response_body',
      '{ total: extra, entries: rows.map((row) => { return { id: row.id, label, size: label.length } }) }',
      'return {',
      "app.get('/summary'"
    );
    assert.ok(text !== undefined, 'the route still has a body');
    assert.notStrictEqual(text, '{ id: string; label: string; size: number; }');
    assert.match(text, /^\{ total: number; entries: /);
  });

  // Controls: what the reverse rule is for still binds.

  it('still binds the call inside a wrapper the locator adds', async () => {
    const text = await infer('Awaited', 'call_result', 'await fetch(`/api/items`)', 'await fetch(`/api/items`)');
    assert.strictEqual(text, HITS);
  });

  it('still finds a call whose text matches exactly', async () => {
    const text = await infer(
      'Exact',
      'call_result',
      'fetch(`/api/search?` + new URLSearchParams({ q: term }))',
      'await fetch(`/api/search?`'
    );
    assert.strictEqual(text, HITS);
  });
});
