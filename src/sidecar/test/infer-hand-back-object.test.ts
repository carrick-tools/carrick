/**
 * Regression for carrick#4185: a call through a wrapper that hands back an
 * object holding the transport response and the parsed body
 * (`return { res, json: (await res.json()) as Reply }`) was published whole
 * as the call's expected response type. Judged against the producer's body,
 * every member of that object (`res`, `json`) read as one the producer does
 * not send: a false `incompatible`.
 *
 * The body is the member the wrapper fills from the response's body read,
 * decided from the wrapper's own code, never from the member's name. Where
 * the wrapper cannot be read (a library hands the object back, or the
 * returns disagree), the site states no response contract
 * (`machinery_envelope`), so the object is never compared.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVICE_TS = `export interface Reply {
  data: { id: number; name: string };
}

export async function call(url: string) {
  const res = await fetch(url);
  return { res, json: (await res.json()) as Reply };
}

export async function callHeld(url: string) {
  const res = await fetch(url);
  const parsed = (await res.json()) as Reply;
  return { res, parsed };
}

export async function callTwoWays(url: string, flag: boolean) {
  const res = await fetch(url);
  if (flag) {
    return { res, first: (await res.json()) as Reply };
  }
  return { res, second: (await res.json()) as Reply };
}

declare function fromLibrary(url: string): Promise<{ res: Response; json: Reply }>;

export async function onSave() {
  const out = await call("/api/reply");
  return out.json.data.name;
}

export async function onSaveHeld() {
  const out = await callHeld("/api/reply");
  return out.parsed.data.id;
}

export async function onSaveTwoWays() {
  const out = await callTwoWays("/api/reply", true);
  return out.res.ok;
}

export async function onSaveLibrary() {
  const out = await fromLibrary("/api/reply");
  return out.json.data.name;
}

declare function useAction<T>(run: () => Promise<T>): { run: () => Promise<T> };

export async function onSaveHook() {
  const action = useAction(() => call("/api/reply"));
  const out = await action.run();
  return out.json.data.name;
}

export async function callBody(url: string) {
  const res = await fetch(url);
  return (await res.json()) as Reply;
}

export async function onSaveBody() {
  const out = await callBody("/api/reply");
  return out.data.name;
}
`;

/** 1-based lines in SERVICE_TS, read off the source above. */
const CALL_LINE = 27;
const HELD_LINE = 32;
const TWO_WAYS_LINE = 37;
const LIBRARY_LINE = 42;
const HOOK_LINE = 50;
const BODY_LINE = 60;

const REPLY_TEXT = '{ data: { id: number; name: string; }; }';

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; kind: string; reason: string }>;
    body_member?: string;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#4185: a hand-back object is not the response body', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4185-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
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
    servicePath = path.join(repoDir, 'src', 'service.ts');
    fs.writeFileSync(servicePath, SERVICE_TS);
    const lines = SERVICE_TS.split('\n');
    assert.match(lines[CALL_LINE - 1], /await call\(/);
    assert.match(lines[HELD_LINE - 1], /await callHeld\(/);
    assert.match(lines[TWO_WAYS_LINE - 1], /await callTwoWays\(/);
    assert.match(lines[LIBRARY_LINE - 1], /await fromLibrary\(/);
    assert.match(lines[HOOK_LINE - 1], /await action\.run\(/);
    assert.match(lines[BODY_LINE - 1], /await callBody\(/);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function infer(
    alias: string,
    line: number,
    expressionText: string,
    result?: { member: string[] }
  ) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: servicePath,
          line_number: line,
          infer_kind: 'call_result',
          alias,
          expression_text: expressionText,
          expression_line: line,
          ...(result ? { result } : {}),
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  const rootReasons = (inferred: { any_provenance?: Array<{ path: string; reason: string }> }) =>
    (inferred.any_provenance ?? []).filter((p) => p.path === '').map((p) => p.reason);

  it('reads the member the wrapper fills from the body read', async () => {
    const inferred = await infer('Endpoint_Reply_Response_Call', CALL_LINE, 'call("/api/reply")');
    assert.ok(inferred, 'the wrapper states the body and the site must publish it');
    assert.strictEqual(collapse(inferred.type_string), REPLY_TEXT);
    // carrick#4196: the answer names the member it read, so a caller row the
    // request summaries own takes it and no other reading of the call.
    assert.strictEqual(inferred.body_member, 'json');
  });

  it('reads the member through a local that holds the body read', async () => {
    const inferred = await infer('Endpoint_Reply_Response_Held', HELD_LINE, 'callHeld("/api/reply")');
    assert.ok(inferred, 'the wrapper states the body and the site must publish it');
    assert.strictEqual(collapse(inferred.type_string), REPLY_TEXT);
    assert.strictEqual(inferred.body_member, 'parsed');
  });

  it('states no contract where the wrapper returns disagree on the member', async () => {
    const inferred = await infer(
      'Endpoint_Reply_Response_TwoWays',
      TWO_WAYS_LINE,
      'callTwoWays("/api/reply", true)'
    );
    assert.ok(inferred, 'the decision must ride the row');
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
    assert.deepStrictEqual(rootReasons(inferred), ['machinery_envelope']);
    assert.strictEqual(inferred.body_member, undefined);
  });

  it('states no contract where no code of the service builds the object', async () => {
    const inferred = await infer(
      'Endpoint_Reply_Response_Library',
      LIBRARY_LINE,
      'fromLibrary("/api/reply")'
    );
    assert.ok(inferred, 'the decision must ride the row');
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
    assert.deepStrictEqual(rootReasons(inferred), ['machinery_envelope']);
    assert.strictEqual(inferred.body_member, undefined);
  });

  it('states no contract where a generic hook hands the object back', async () => {
    const inferred = await infer('Endpoint_Reply_Response_Hook', HOOK_LINE, 'action.run()');
    assert.ok(inferred, 'the decision must ride the row');
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
    assert.deepStrictEqual(rootReasons(inferred), ['machinery_envelope']);
    assert.strictEqual(inferred.body_member, undefined);
  });

  // carrick#4196: only the hand-back reader marks its answer. A member the
  // library store's label names and a call whose value is the body itself
  // are answered without the marker.
  it('marks no member the library label names', async () => {
    const inferred = await infer(
      'Endpoint_Reply_Response_Labelled',
      LIBRARY_LINE,
      'fromLibrary("/api/reply")',
      { member: ['json'] }
    );
    assert.ok(inferred, 'the label names a member the type has');
    assert.strictEqual(collapse(inferred.type_string), REPLY_TEXT);
    assert.strictEqual(inferred.body_member, undefined);
  });

  it('marks no call whose value is the body itself', async () => {
    const inferred = await infer('Endpoint_Reply_Response_Body', BODY_LINE, 'callBody("/api/reply")');
    assert.ok(inferred, 'the wrapper hands back the body');
    assert.strictEqual(collapse(inferred.type_string), 'Reply');
    assert.strictEqual(inferred.body_member, undefined);
  });
});
