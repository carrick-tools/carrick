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
import { canonicalStringQuotes } from '../src/capture/site-node.js';

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

export async function twoTemplates(origin: string, path: string, token: string) {
  const res = await fetch(\`\${origin}\${path}\`, {
    headers: { Authorization: \`Bearer \${token}\` },
  });
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

  it('still finds a call located by its first line when the call holds two templates', async () => {
    const text = await infer(
      'TwoTemplates',
      'call_result',
      'fetch(`${origin}${path}`, {',
      'await fetch(`${origin}${path}`'
    );
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

describe('carrick#2056: string literals compare by value, not by quotes', () => {
  it('writes every plain literal in double quotes', () => {
    assert.strictEqual(canonicalStringQuotes("f('/a', \"/b\", `/c`)"), 'f("/a", "/b", "/c")');
  });

  it('pairs quotes left to right, so two templates never pair with each other', () => {
    const text = 'f(`${o}${p}`, {h: `Bearer ${t}`}) + `/x`';
    assert.strictEqual(canonicalStringQuotes(text), 'f(`${o}${p}`, {h: `Bearer ${t}`}) + "/x"');
  });

  it('copies a literal holding a quote, an escape or an interpolation as it is', () => {
    for (const text of ['"it\'s"', "'a\\'b'", '`a${"b"}c`', "'unterminated"]) {
      assert.strictEqual(canonicalStringQuotes(text), text);
    }
  });
});

/**
 * carrick#2114: a call located by its first line alone bound a shorter
 * sibling call.
 *
 * A wrapped call's locator is its first line (`load({`). No node has that
 * text, so every node holding it is a candidate, and the smallest won: a
 * shorter call of the same opening a few lines away, whatever line the
 * locator stated.
 *
 * Among the nodes that hold the text, the ones whose own text OPENS with it
 * are the ones a first-line locator names. Where there is one, the nearest to
 * the stated line wins, then the smallest. Where there is none (a fragment
 * from the middle of a node) the smallest holder wins, as before.
 */
const SIBLINGS_TS = `interface Totals { total: number }
interface Thing { id: string }
interface Other { label: string }
declare const api: {
  getTotals(id: string): Promise<Totals>;
  getTotalsByDay(id: string): Promise<Totals[]>;
};
declare function load<T>(o: { run: () => Promise<T>; staleMs: number }): Promise<T>;
declare function retry<T>(run: () => Promise<T>): Promise<T>;
declare function boxed<T>(value: T): { inner: T };
declare function useThing<T>(o: { key: string; read: () => T }): { data: T };
declare const client: { send(scope: string, kind: string, value: string): Promise<{ ok: boolean }> };
declare const thing: Thing;
declare const other: Other;

export async function both(id: string) {
  const [totals, byDay] = await Promise.all([
    load({ // first
      run: () => api.getTotals(id),
      staleMs: 1,
    }),
    load({ // second
      run: () => api.getTotalsByDay(id), // second property
      staleMs: 1,
    }),
  ]);
  console.log(totals, byDay);
}

export async function wrapped(id: string) {
  const r = await retry(() => // retry line
    load({
      run: () => api.getTotalsByDay(id),
      staleMs: 1,
    })
  );
  console.log(r);
}

export function nested(seed: number) {
  const twice = boxed(boxed(seed));
  console.log(twice);
}

export async function push(scope: string, value: string) {
  await client.send(scope, 'kind', value);
  const sent = { scope, kind: 'kind', value };
  console.log(sent);
}

export function hooks() {
  const a = useThing({ // hook a
    key: 'a',
    read: () => thing,
  });
  const b = useThing({ // hook b
    key: 'the-longer-b',
    read: () => other,
  });
  console.log(a, b);
}
`;

describe('carrick#2114: a first-line locator binds the call on its own line', () => {
  let client: SidecarClient;
  let repoDir: string;
  let sourcePath: string;

  const siblingLine = (marker: string): number => {
    const index = SIBLINGS_TS.split('\n').findIndex((line) => line.includes(marker));
    assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
    return index + 1;
  };

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2114-'));
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
    sourcePath = path.join(repoDir, 'src', 'siblings.ts');
    fs.writeFileSync(sourcePath, SIBLINGS_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-2114', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (
    alias: string,
    kind: 'call_result' | 'request_body',
    expression: string,
    at: string
  ): Promise<string | undefined> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: sourcePath,
          line_number: siblingLine(at),
          expression_text: expression,
          expression_line: siblingLine(at),
          infer_kind: kind,
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    return inferred ? collapse(inferred.type_string) : undefined;
  };

  it('reads the longer of two sibling calls at its own line', async () => {
    assert.strictEqual(await infer('Second', 'call_result', 'load({', '// second'), 'Totals[]');
  });

  it('reads the shorter sibling at its own line', async () => {
    assert.strictEqual(await infer('First', 'call_result', 'load({', '// first'), 'Totals');
  });

  it('binds the nearer call when the stated line is one off', async () => {
    assert.strictEqual(
      await infer('OffByOne', 'call_result', 'load({', '// second property'),
      'Totals[]'
    );
  });

  it('does not prefer a wrapper that starts on the stated line but does not open with the text', async () => {
    assert.strictEqual(await infer('Wrapped', 'call_result', 'load({', '// retry line'), 'Totals[]');
  });

  it('still binds the innermost of two nested calls on one line', async () => {
    assert.strictEqual(await infer('Nested', 'call_result', 'boxed(', 'boxed(boxed(seed))'), '{ inner: number; }');
  });

  it('leaves a fragment from the middle of an argument list on the smallest node holding it', async () => {
    // Nothing opens with this text, so the rule for a first line does not
    // apply and the stated line decides nothing: a rule that preferred any
    // node starting on that line would bind the call, and publish its
    // response as the request.
    assert.strictEqual(
      await infer('Fragment', 'request_body', "'kind', value", "client.send(scope, 'kind', value)"),
      '{ scope: string; kind: string; value: string; }'
    );
  });

  it('reads each of two calls that open alike as its own', async () => {
    assert.strictEqual(await infer('HookB', 'call_result', 'useThing({', '// hook b'), '{ data: Other; }');
    assert.strictEqual(await infer('HookA', 'call_result', 'useThing({', '// hook a'), '{ data: Thing; }');
  });
});
