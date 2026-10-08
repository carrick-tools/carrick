/**
 * carrick#1841: a symbol the model names for a consumer's response that is a
 * response table keyed by status code (`GetItemResponses = { 200: Item }`,
 * the call's first type argument in a generated client) publishes the
 * table's 2xx body, not the table.
 *
 * An explicit symbol outranks inference in both places that resolve it: the
 * v1 bundle (which the index's type tools read) and the capture's symbol
 * anchor (which the check reads). Both read the rule, so both answers agree.
 * The request says the symbol names a consumer response; nothing reads the
 * alias spelling or the symbol's name.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'status-table-client');
const TYPES = 'src/types.gen.ts';

interface BundleShape {
  status: string;
  dts_content?: string;
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

/** The right-hand side of `export type <alias> = …;` in a bundle or surface. */
function aliasLine(text: string, alias: string): string {
  const match = new RegExp(`export (?:type|interface) ${alias}\\b[^\\n]*`).exec(text);
  assert.ok(match, `no declaration of ${alias} in:\n${text}`);
  return collapse(match[0]);
}

describe('carrick#1841: a status-table symbol named for a consumer response', () => {
  let repoDir: string;
  let outRoot: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1841-sym-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1841-stub-'));
    fs.cpSync(FIXTURE, repoDir, { recursive: true });
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  describe('v1 bundle', () => {
    let client: SidecarClient;
    let dts: string;

    before(async () => {
      client = new SidecarClient();
      await client.start();
      await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
      const bundle = await client.send<BundleShape>({
        action: 'bundle',
        request_id: 'bundle',
        symbols: [
          { symbol_name: 'GetItemResponses', source_file: TYPES, alias: 'Get_Response', consumer_response: true },
          { symbol_name: 'ListItemsResponses', source_file: TYPES, alias: 'List_Response', consumer_response: true },
          { symbol_name: 'DeleteItemResponses', source_file: TYPES, alias: 'Delete_Response', consumer_response: true },
          { symbol_name: 'GetItemResponses', source_file: TYPES, alias: 'Producer_Table' },
          { symbol_name: 'Item', source_file: TYPES, alias: 'Item_Response', consumer_response: true },
        ],
      });
      assert.strictEqual(bundle.status, 'success', JSON.stringify(bundle.errors));
      dts = bundle.dts_content ?? '';
    });

    after(async () => {
      await client.stop();
    });

    it('bundles the success body, not the table', () => {
      assert.strictEqual(
        aliasLine(dts, 'Get_Response'),
        'export type Get_Response = { id: string; name: string; };'
      );
      assert.strictEqual(
        aliasLine(dts, 'List_Response'),
        'export type List_Response = { id: string; name: string; }[];'
      );
    });

    it('a table whose only success row is 204 states no body', () => {
      assert.strictEqual(aliasLine(dts, 'Delete_Response'), 'export type Delete_Response = unknown;');
    });

    it('control: the table is bundled as written where no consumer response is marked', () => {
      assert.match(aliasLine(dts, 'Producer_Table'), /\b200:/);
    });

    it('control: a marked symbol that is no table is bundled as written', () => {
      assert.match(aliasLine(dts, 'Item_Response'), /Item_Response \{ id: string; name: string; \}/);
    });
  });

  describe('capture symbol anchor', () => {
    let result: CaptureStubResult;
    let surface: string;

    before(() => {
      const anchor = (alias: string, symbol_name: string, consumer_response?: boolean) => ({
        kind: 'symbol' as const,
        alias,
        symbol_name,
        source_file: TYPES,
        anchor_origin: 'llm-symbol' as const,
        ...(consumer_response ? { consumer_response } : {}),
      });
      result = captureStub({
        repoRoot: repoDir,
        serviceName: 'status-table-client',
        outDir: path.join(outRoot, 'stub'),
        anchors: [
          anchor('Get_Response', 'GetItemResponses', true),
          anchor('Delete_Response', 'DeleteItemResponses', true),
          anchor('Producer_Table', 'GetItemResponses'),
        ],
      });
      assert.strictEqual(result.success, true, JSON.stringify(result.errors));
      surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
    });

    function record(alias: string) {
      const r = result.aliases.find((a) => a.alias === alias);
      assert.ok(r, `no alias record for ${alias}`);
      return r;
    }

    it('captures the success row of the table, which the compiler emits', () => {
      assert.match(aliasLine(surface, 'Get_Response'), /\.GetItemResponses\[200\];$/);
      const r = record('Get_Response');
      assert.strictEqual(r.serialization, 'emitted');
      assert.strictEqual(r.self_check, 'ok', r.self_check_detail);
    });

    it('a table whose only success row is 204 abstains, decided', () => {
      assert.strictEqual(aliasLine(surface, 'Delete_Response'), 'export type Delete_Response = unknown;');
      const r = record('Delete_Response');
      assert.strictEqual(r.capture_failure_reason, undefined, 'an abstain is not a demotion');
      assert.match(r.self_check_detail ?? '', /response table keyed by status code/);
    });

    it('control: an unmarked table anchor captures the symbol as written', () => {
      assert.match(aliasLine(surface, 'Producer_Table'), /\.GetItemResponses;$/);
    });
  });
});
