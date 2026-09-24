/**
 * carrick-cloud#1366: three request-body shapes that published the wrong
 * contract and read as enforced risks.
 *
 *  1. A handler parameter a KEYED decorator binds to one body field
 *     (`@Body('masterKey') masterKey: string`) published the field's type as
 *     the whole body (`string`), so every consumer sending
 *     `{ masterKey: string }` read "not assignable to string".
 *  2. A consumer request locator that names the request CALL
 *     (`api.post<Result>(url, body)`) published the call's response as the
 *     request body.
 *  3. A consumer `delete(url, { data })` located on the request CONFIG
 *     published the config (`{ data: {...} }`) as the body.
 *
 * Each shape is pinned twice: by what `infer` returns for the real locator,
 * and by what the tsc judge says when those inferred texts are compared. The
 * judge half proves the false risk is gone AND that a real mismatch of the
 * same shape is still caught.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
import { runCheck } from '../src/capture/index.js';
import type { CheckPairSpec, CheckVerdict } from '../src/capture/api.js';

const PRODUCER = path.join(FIXTURES_PATH, 'src/keyed-body-params.ts');
const CONSUMER = path.join(FIXTURES_PATH, 'src/request-config-body.ts');

function lineOf(file: string, anchor: string): number {
  const source = fs.readFileSync(file, 'utf-8');
  const idx = source.indexOf(anchor);
  assert.ok(idx >= 0, `fixture must contain: ${anchor}`);
  assert.strictEqual(source.indexOf(anchor, idx + 1), -1, `ambiguous anchor: ${anchor}`);
  return source.slice(0, idx).split('\n').length;
}

interface InferResponseShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; reason: string }>;
  }>;
  errors?: string[];
}

describe('request-body shapes (carrick-cloud#1366)', () => {
  let client: SidecarClient;
  const inferred = new Map<string, string>();

  async function infer(
    alias: string,
    file: string,
    expressionText: string,
    anchor: string
  ): Promise<{ type_string: string; reasons: string[] }> {
    const line = lineOf(file, anchor);
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: `shapes-1366-${alias}`,
      requests: [
        {
          file_path: file,
          line_number: line,
          expression_text: expressionText,
          expression_line: line,
          infer_kind: 'request_body',
          alias,
        },
      ],
    });
    const row = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(row, `no inferred type for ${alias}: ${JSON.stringify(response.errors)}`);
    inferred.set(alias, row.type_string);
    return {
      type_string: row.type_string,
      reasons: (row.any_provenance ?? []).map((p) => p.reason),
    };
  }

  /** Like `infer`, but a locator that abstains returns no row instead of failing. */
  async function inferMaybe(
    alias: string,
    file: string,
    expressionText: string,
    anchor: string
  ): Promise<{ type_string: string; reasons: string[] } | undefined> {
    const line = lineOf(file, anchor);
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: `shapes-1366-${alias}`,
      requests: [
        {
          file_path: file,
          line_number: line,
          expression_text: expressionText,
          expression_line: line,
          infer_kind: 'request_body',
          alias,
        },
      ],
    });
    const row = response.inferred_types?.find((t) => t.alias === alias);
    return row
      ? { type_string: row.type_string, reasons: (row.any_provenance ?? []).map((p) => p.reason) }
      : undefined;
  }

  before(async () => {
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'shapes-1366-init', repo_root: FIXTURES_PATH });
  });

  after(async () => {
    await client.stop();
  });

  describe('1. keyed body parameter', () => {
    it('the parameter name publishes an object with that one field', async () => {
      const got = await infer('AuthByName', PRODUCER, 'masterKey', "auth(@Body('masterKey')");
      assert.strictEqual(got.type_string, '{ masterKey: string; }');
    });

    it('the whole decorated parameter publishes the same object', async () => {
      const got = await infer(
        'AuthByParam',
        PRODUCER,
        "@Body('masterKey') masterKey: string",
        "auth(@Body('masterKey')"
      );
      assert.strictEqual(got.type_string, '{ masterKey: string; }');
    });

    it('every keyed read of the same decorator merges, optional stays optional, the path key does not join', async () => {
      const got = await infer('StatusMerged', PRODUCER, 'status', "@Body('status')");
      assert.strictEqual(got.type_string, '{ status: "active" | "suspended"; note?: string; }');
    });

    it('control: an unkeyed body parameter keeps its own type', async () => {
      const got = await infer('WholeBody', PRODUCER, 'dto', 'create(@Body() dto');
      assert.strictEqual(got.type_string, '{ name: string; size: number; }');
    });

    it('control: a member read of the parameter is not the parameter', async () => {
      const got = await infer('MemberRead', PRODUCER, 'masterKey.length', 'masterKey.length');
      assert.strictEqual(got.type_string, 'number');
    });
  });

  describe('1b. keyed body parameter beside a whole-body parameter, pipes, other decorators', () => {
    it('a whole-body sibling and a keyed one publish their intersection', async () => {
      const got = await infer('DtoAndKeyed', PRODUCER, 'force', "@Body('force')");
      assert.strictEqual(got.type_string, '{ name: string; size: number; } & { force: boolean; }');
    });

    it('a pipe as the first argument is the whole-body form', async () => {
      const got = await infer('PipeWhole', PRODUCER, 'checked', '@Body(new CheckPipe()) checked');
      assert.strictEqual(got.type_string, '{ name: string; size: number; }');
    });

    it('a piped whole body and a keyed field publish their intersection', async () => {
      const got = await infer('PipeAndKeyed', PRODUCER, 'dryRun', "@Body('dryRun')");
      assert.strictEqual(got.type_string, '{ name: string; size: number; } & { dryRun: boolean; }');
    });

    it('another decorator before the keyed one does not hide the key', async () => {
      const got = await infer('LeadingDecorator', PRODUCER, 'label', "@Body('label')");
      assert.strictEqual(got.type_string, '{ label: string; }');
    });
  });

  describe('2. locator on the request call', () => {
    it('publishes the body argument, not the response', async () => {
      const got = await infer(
        'QueryCall',
        CONSUMER,
        "api.post<QueryResultData>('/ops/query', { sql })",
        "api.post<QueryResultData>('/ops/query', { sql })"
      );
      assert.strictEqual(got.type_string, '{ sql: string; }');
    });

    it('control: a body literal that happens to carry a `data` key is the body', async () => {
      const got = await infer('EnvelopeBody', CONSUMER, "{ data: 1, label: 'x' }", "label: 'x'");
      assert.strictEqual(got.type_string, '{ data: number; label: string; }');
    });
  });

  describe('3. request config located instead of the body', () => {
    it('publishes the config payload member', async () => {
      const got = await infer('DeleteConfig', CONSUMER, '{ data: { reason } }', '`/accounts/${id}`, { data: { reason } }');
      assert.strictEqual(got.type_string, '{ reason: string; }');
    });

    it('a config with no payload member is a decided no-body abstain', async () => {
      const got = await infer(
        'DeleteNoBody',
        CONSUMER,
        "{ params: { quiet: 'true' } }",
        "params: { quiet: 'true' }"
      );
      assert.strictEqual(got.type_string, 'unknown');
      assert.deepStrictEqual(got.reasons, ['no_request_body']);
    });
  });

  describe('3b. config variables, a static client, and option shapes that carry no generic body', () => {
    it('a config variable publishes its payload member, located on the variable', async () => {
      const got = await infer('BuiltConfigVar', CONSUMER, 'removal', '`/accounts/${id}`, removal');
      assert.strictEqual(got.type_string, '{ reason: string; }');
    });

    it('a config variable publishes its payload member, located on the call', async () => {
      const got = await infer(
        'BuiltConfigCall',
        CONSUMER,
        'api.delete(`/accounts/${id}`, removal)',
        '`/accounts/${id}`, removal'
      );
      assert.strictEqual(got.type_string, '{ reason: string; }');
    });

    it('a config variable whose type does not resolve the member abstains', async () => {
      const got = await inferMaybe('TypedConfigVar', CONSUMER, 'opaque', '`/accounts/${id}`, opaque');
      assert.strictEqual(got, undefined, JSON.stringify(got));
    });

    it('a client called through its static default reads the config the same way', async () => {
      const got = await infer(
        'StaticClient',
        CONSUMER,
        "{ data: { reason, via: 'static' } }",
        "via: 'static'"
      );
      assert.strictEqual(got.type_string, '{ reason: string; via: string; }');
    });

    it('options generic over a response mode: the mode is never the body, and no body is not decided', async () => {
      for (const [alias, text, anchor] of [
        ['ModeOptions', "{ body: note, responseType: 'json' }", "{ body: note, responseType: 'json' }"],
        ['MethodOptions', "{ method: 'POST', body: note }", "{ method: 'POST', body: note }"],
      ]) {
        const got = await inferMaybe(alias, CONSUMER, text, anchor);
        assert.strictEqual(got, undefined, `${alias}: ${JSON.stringify(got)}`);
      }
    });

    it('options generic over a response mode: a located call is unchanged', async () => {
      const got = await infer(
        'ModeCall',
        CONSUMER,
        "fetchData('/notes/raw', { method: 'POST', body: note })",
        "{ method: 'POST', body: note }"
      );
      assert.strictEqual(got.type_string, 'unknown');
      assert.deepStrictEqual(got.reasons, []);
    });

    it('non-generic json options are unchanged', async () => {
      const got = await infer('ShortClient', CONSUMER, '{ json: note }', '{ json: note }');
      assert.strictEqual(got.type_string, '{ json: { text: string; }; }');
    });

    it('non-generic json options with a response mode are unchanged', async () => {
      const got = await infer(
        'StreamClient',
        CONSUMER,
        "{ json: note, responseType: 'json' }",
        "{ json: note, responseType: 'json' }"
      );
      assert.strictEqual(got.type_string, '{ json: { text: string; }; responseType: "json"; }');
    });
  });

  describe('the judge over the inferred texts', () => {
    let verdicts: Map<string, CheckVerdict>;
    let root: string;

    before(async () => {
      // Producer and consumer rows for each pair, inferred from the real
      // locators rather than typed in, so the judge sees what a scan ships.
      await infer('P_Auth', PRODUCER, 'masterKey', "auth(@Body('masterKey')");
      await infer('P_Remove', PRODUCER, 'reason', "@Body('reason')");
      await infer('P_Query', PRODUCER, 'sql', "@Body('sql')");
      await infer('C_Auth', CONSUMER, '{ masterKey }', '{ masterKey }');
      await infer('C_AuthPin', CONSUMER, '{ masterKey: pin }', '{ masterKey: pin }');
      await infer('C_Remove', CONSUMER, '{ data: { reason } }', '`/accounts/${id}`, { data: { reason } }');
      await infer(
        'C_RemoveCode',
        CONSUMER,
        '{ data: { reasonCode: code } }',
        '{ data: { reasonCode: code } }'
      );
      await infer(
        'C_Query',
        CONSUMER,
        "api.post<QueryResultData>('/ops/query', { sql })",
        "api.post<QueryResultData>('/ops/query', { sql })"
      );
      await infer(
        'C_QueryMisnamed',
        CONSUMER,
        "api.post<QueryResultData>('/ops/query', { query: sql })",
        "api.post<QueryResultData>('/ops/query', { query: sql })"
      );

      root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-shapes-1366-'));
      const write = (service: string, aliases: string[]) => {
        const dir = path.join(root, service);
        fs.mkdirSync(path.join(dir, 'types'), { recursive: true });
        fs.writeFileSync(
          path.join(dir, 'package.json'),
          JSON.stringify({
            name: `@carrick/${service}`,
            version: '0.0.0-carrick',
            private: true,
            types: './types/surface.d.ts',
          }) + '\n'
        );
        fs.writeFileSync(
          path.join(dir, 'types', 'surface.d.ts'),
          aliases.map((a) => `export type ${a} = ${inferred.get(a)};`).join('\n') + '\n'
        );
      };
      write('ops', ['P_Auth', 'P_Remove', 'P_Query']);
      write('web', ['C_Auth', 'C_AuthPin', 'C_Remove', 'C_RemoveCode', 'C_Query', 'C_QueryMisnamed']);

      const pair = (key: string, producer: string, consumer: string): CheckPairSpec => ({
        pair_key: key,
        protocol: 'http',
        type_kind: 'request',
        producer: { service_name: 'ops', alias: producer },
        consumer: { service_name: 'web', alias: consumer },
      });
      const result = await runCheck({
        stubs: [
          { service_name: 'ops', stub_dir: path.join(root, 'ops') },
          { service_name: 'web', stub_dir: path.join(root, 'web') },
        ],
        pairs: [
          pair('auth', 'P_Auth', 'C_Auth'),
          pair('auth-wrong', 'P_Auth', 'C_AuthPin'),
          pair('remove', 'P_Remove', 'C_Remove'),
          pair('remove-wrong', 'P_Remove', 'C_RemoveCode'),
          pair('query', 'P_Query', 'C_Query'),
          pair('query-wrong', 'P_Query', 'C_QueryMisnamed'),
        ],
      });
      assert.strictEqual(result.success, true, JSON.stringify(result.errors));
      verdicts = new Map(result.verdicts.map((v) => [v.pair_key, v]));
    });

    after(() => {
      if (root) fs.rmSync(root, { recursive: true, force: true });
    });

    for (const [key, shape] of [
      ['auth', 'keyed body parameter'],
      ['remove', 'delete config body'],
      ['query', 'locator on the request call'],
    ] as const) {
      it(`${shape}: a matching consumer is compatible`, () => {
        const v = verdicts.get(key)!;
        assert.strictEqual(v.bucket, 'compatible', `${key}: ${v.diagnostic}`);
      });

      it(`${shape}: a real mismatch of the same shape is still caught`, () => {
        const v = verdicts.get(`${key}-wrong`)!;
        assert.strictEqual(v.bucket, 'incompatible', `${key}-wrong: ${JSON.stringify(v)}`);
      });
    }
  });
});
