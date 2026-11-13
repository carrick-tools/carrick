/**
 * carrick#4232: a `call_result` anchor reads a call's result, so its locator
 * must land on a call. The capture's text tier binds the node a locator's text
 * names exactly, and its line tier the first expression on the line; on a
 * line holding JSX both land on the element, and the element's own type was
 * published as the call's response.
 *
 * The inferrer refuses a call-result locator that finds no call (#2056). The
 * capture reads the same rule (`callNamedBy`): a located node that is not a
 * call, after the wrappers that leave a call's value as it is (`await`,
 * parentheses, `as`, `!`, `satisfies`, a variable's initializer), abstains.
 * Anchors of any other kind keep their answers.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord, InferAnchorRequest } from '../src/capture/api.js';
import { SidecarClient } from './helpers.js';

const SEARCH_TSX = `declare global {
  namespace JSX {
    interface Element { type: any; props: any; key: string | null }
    interface IntrinsicElements { [name: string]: any }
  }
}
export interface SearchResult { hits: string[]; total: number }
declare function useSubmitter<T>(): {
  Form: (props: { action: string; method: string; children?: unknown }) => JSX.Element;
};
declare const api: { post<T>(url: string, body: unknown): Promise<T> };

export function SearchBox({ id }: { id: string }) {
  const form = useSubmitter<SearchResult>();
  return (
    <form.Form action={\`/things/\${id}/search\`} method="post">
      <input name="q" />
    </form.Form>
  );
}

export function QuickSearch({ id }: { id: string }) {
  return (
    <form onSubmit={() => fetch(\`/things/\${id}/search\`, { method: 'POST' })}>
      <input name="q" />
    </form>
  );
}

export async function runSearch(id: string, body: { q: string }) {
  const found = await api.post<SearchResult>(\`/things/\${id}/search\`, body);
  return found;
}
`;

const LINES = SEARCH_TSX.split('\n');
function lineOf(fragment: string): number {
  const hits = LINES.flatMap((l, i) => (l.includes(fragment) ? [i + 1] : []));
  assert.strictEqual(hits.length, 1, `fixture must hold one line with: ${fragment}`);
  return hits[0];
}

const FORM_TAG = '<form.Form action={`/things/${id}/search`} method="post">';
const FETCH_TAG = "<form onSubmit={() => fetch(`/things/${id}/search`, { method: 'POST' })}>";
const POST_CALL = 'api.post<SearchResult>(`/things/${id}/search`, body)';

type Anchor = Omit<InferAnchorRequest, 'kind' | 'alias' | 'source_file' | 'anchor_origin'>;

/** call_result anchors that land on no call. */
const REFUSED: Record<string, Anchor> = {
  Call_FormTag: { infer_kind: 'call_result', expression_text: FORM_TAG, line_number: lineOf('<form.Form') },
  Call_FetchTag: { infer_kind: 'call_result', expression_text: FETCH_TAG, line_number: lineOf('<form onSubmit') },
  Call_FormLine: { infer_kind: 'call_result', line_number: lineOf('<form.Form') },
};

/** call_result anchors that land on the call or a wrapper of it. */
const READ: Record<string, Anchor> = {
  Call_PostText: { infer_kind: 'call_result', expression_text: POST_CALL, line_number: lineOf('api.post<') },
  Call_PostAwait: { infer_kind: 'call_result', expression_text: `await ${POST_CALL}`, line_number: lineOf('api.post<') },
  Call_PostLine: { infer_kind: 'call_result', line_number: lineOf('api.post<') },
};

/** The same JSX locator on an anchor of another kind keeps its answer. */
const OTHER_KIND: Record<string, Anchor> = {
  Expr_FormTag: { infer_kind: 'expression', expression_text: FORM_TAG, line_number: lineOf('<form.Form') },
};

describe('carrick#4232: a call-result anchor binds only a call', () => {
  let repoDir: string;
  let outRoot: string;
  let surface: string;
  let records: Map<string, CaptureAliasRecord>;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4232-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4232-stub-'));
    fs.mkdirSync(path.join(repoDir, 'src'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          jsx: 'preserve',
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'search.tsx'), SEARCH_TSX);

    const anchors = { ...REFUSED, ...READ, ...OTHER_KIND };
    const result = captureStub({
      repoRoot: repoDir,
      serviceName: 'search-ui',
      outDir: path.join(outRoot, 'stub'),
      anchors: Object.entries(anchors).map(([alias, anchor]) => ({
        kind: 'infer' as const,
        alias,
        source_file: 'src/search.tsx',
        anchor_origin: 'deterministic-infer' as const,
        ...anchor,
      })),
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
    records = new Map(result.aliases.map((a) => [a.alias, a]));
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  function declared(alias: string): string {
    const m = surface.match(
      new RegExp(`export type ${alias} = ([\\s\\S]*?);(?=\\s*(?:export type |$))`)
    );
    assert.ok(m, `no surface line for ${alias}:\n${surface}`);
    return m![1].replace(/\s+/g, ' ').trim();
  }

  for (const alias of Object.keys(REFUSED)) {
    it(`${alias} abstains as unknown`, () => {
      assert.strictEqual(declared(alias), 'unknown', `published the element: ${declared(alias)}`);
      const record = records.get(alias);
      assert.ok(record, `no record for ${alias}`);
      assert.strictEqual(record.serialization, 'structural_fallback');
      // An abstain, not a demotion: nothing for the backfill to re-anchor.
      assert.strictEqual(record.capture_failure_reason, undefined);
      assert.strictEqual(record.unread_cause, 'not_located');
      assert.match(record.self_check_detail ?? '', /names no call/);
    });
  }

  for (const alias of Object.keys(READ)) {
    it(`${alias} reads the call's result`, () => {
      assert.match(declared(alias), /\bSearchResult$/);
    });
  }

  it('an expression anchor on the same JSX locator keeps its answer', () => {
    assert.strictEqual(declared('Expr_FormTag'), 'JSX.Element');
  });
});

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

describe('carrick#4232: the inferrer refuses the same call-result locators', () => {
  let client: SidecarClient;
  let repoDir: string;
  let sourcePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4232-infer-'));
    fs.mkdirSync(path.join(repoDir, 'src'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          jsx: 'preserve',
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    sourcePath = path.join(repoDir, 'src', 'search.tsx');
    fs.writeFileSync(sourcePath, SEARCH_TSX);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function inferred(alias: string, anchor: Anchor): Promise<string | undefined> {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: sourcePath,
          line_number: anchor.line_number,
          expression_text: anchor.expression_text,
          expression_line: anchor.line_number,
          infer_kind: anchor.infer_kind,
          alias,
        },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias)?.type_string;
  }

  // Only the tag that holds no call. The inferrer's text search also binds a
  // call the locator's text contains (#335), so on `Call_FetchTag` it reads the
  // `fetch` inside the attribute: a call, which the rule accepts. The capture
  // binds an exact text match alone (#3500) and finds the element there.
  it('Call_FormTag publishes nothing', async () => {
    assert.strictEqual(await inferred('Call_FormTag', REFUSED.Call_FormTag), undefined);
  });

  it('Call_PostText reads the call', async () => {
    assert.strictEqual(await inferred('Call_PostText', READ.Call_PostText), 'SearchResult');
  });
});
