/**
 * carrick#1162: a consumer response typed from a fragment of its call.
 *
 * A call written as a member chain breaks before the dot:
 *
 *     client
 *       .list({ source: 'system' })
 *       .then(({ tasks }) => …)
 *
 * The analyzer reports the call as `client.list({ source: 'system' })`. The text
 * locator collapsed whitespace to single spaces, so the source read
 * `client .list(…)` and never matched exactly; the substring fallback then bound
 * to the call's ARGUMENT, and the response was published as `{ source: string }`
 * (or a `string` from inside it) and judged against the producer's real body.
 *
 * A line break before a member access carries no meaning, so both sides of the
 * comparison drop whitespace around the dot. The response is then the value
 * the call site reads: the awaited result the `.then` callback receives.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { captureStub } from '../src/capture/index.js';

const CLIENT_TS = `export interface TaskPage {
  tasks: string[];
  nextCursor: string | null;
}

export interface TaskClient {
  list(query: { source: string; after?: string }): Promise<TaskPage>;
}

export function loadFirst(client: TaskClient, onPage: (page: TaskPage) => void) {
  client
    .list({ source: "system" })
    .then(({ tasks, nextCursor }) => onPage({ tasks, nextCursor }))
    .catch(() => undefined);
}

export function loadMore(client: TaskClient, cursor: string) {
  client
    ?.list({ source: "system", after: cursor })
    .then((page) => page.tasks.length);
}
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

function collapse(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

describe('carrick#1162 member-chain call locators', () => {
  let repoDir: string;
  let filePath: string;
  let client: SidecarClient;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1162-chain-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler' },
        include: ['src'],
      })
    );
    filePath = path.join(repoDir, 'src', 'client.ts');
    fs.writeFileSync(filePath, CLIENT_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  function lineOf(text: string): number {
    const at = CLIENT_TS.indexOf(text);
    assert.ok(at >= 0, `fixture must contain: ${text}`);
    return CLIENT_TS.slice(0, at).split('\n').length;
  }

  async function callResult(alias: string, expression: string, lineText: string) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: filePath,
          line_number: lineOf(lineText),
          infer_kind: 'call_result',
          alias,
          expression_text: expression,
          expression_line: lineOf(lineText),
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('reads the awaited result of a call broken before its dot', async () => {
    const inferred = await callResult('FirstPage', 'client.list({ source: "system" })', '.list({ source: "system" })');
    assert.ok(inferred, 'the call must resolve');
    // A call result prints the awaited type by name; the anchor symbol carries it.
    assert.strictEqual(collapse(inferred.type_string), 'TaskPage');
  });

  it('reads through an optional-chain break the same way', async () => {
    const inferred = await callResult(
      'MorePages',
      'client?.list({ source: "system", after: cursor })',
      '?.list({ source: "system", after: cursor })'
    );
    assert.ok(inferred, 'the call must resolve');
    assert.ok(
      !/source/.test(inferred.type_string),
      `the argument is not the response, got: ${inferred.type_string}`
    );
    assert.strictEqual(collapse(inferred.type_string), 'TaskPage');
  });

  it('the capture locator matches the broken chain too, instead of a line fallback', () => {
    // The capture re-runs the locator when the v1 text is unusable. Its text
    // match failed the same way, and its line fallback then printed the first
    // expression on the line: the call's argument object.
    const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1162-chain-out-'));
    const result = captureStub({
      repoRoot: repoDir,
      serviceName: 'member-chain',
      outDir,
      anchors: [
        {
          kind: 'infer',
          alias: 'Call_firstPage_Response',
          source_file: 'src/client.ts',
          anchor_origin: 'deterministic-infer',
          line_number: lineOf('.list({ source: "system" })'),
          expression_text: 'client.list({ source: "system" })',
        },
      ],
    });
    assert.ok(result.success, JSON.stringify(result.errors));
    const record = result.aliases.find((a) => a.alias === 'Call_firstPage_Response');
    assert.ok(record);
    const surface = fs.readFileSync(path.join(outDir, 'types/surface.d.ts'), 'utf-8');
    const start = surface.indexOf('export type Call_firstPage_Response =');
    const declaration = surface.slice(start).replace(/\s+/g, ' ');
    assert.ok(!/source/.test(declaration.split(';')[0]), `the argument is not the response: ${declaration}`);
    assert.match(declaration, /TaskPage|tasks: string\[\]/, declaration);
  });
});
