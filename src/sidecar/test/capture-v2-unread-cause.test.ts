/**
 * carrick#3995: the driver says why it sent a literal placeholder
 * (`unread_cause`), and the capture copies the word onto the alias's record,
 * where the scan reads it to say why an entry is untyped. The record carries
 * it whether the alias was demoted or checked, the request validator keeps
 * it, and it never enters the alias's `surface_hash`: no verdict reads it.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord, LiteralAnchorRequest } from '../src/capture/api.js';
import { parseRequest } from '../src/validators.js';

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3995-cause-'));

function capture(name: string, anchors: LiteralAnchorRequest[]): Map<string, CaptureAliasRecord> {
  const repoRoot = path.join(root, `${name}-repo`);
  fs.mkdirSync(repoRoot, { recursive: true });
  const result = captureStub({
    repoRoot,
    serviceName: name,
    outDir: path.join(root, `${name}-stub`),
    anchors,
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  const stubRecords = JSON.parse(
    fs.readFileSync(path.join(result.stub_dir, 'carrick-manifest.json'), 'utf8')
  ) as { aliases: CaptureAliasRecord[] };
  return new Map(stubRecords.aliases.map((record) => [record.alias, record]));
}

const placeholder = (alias: string, text = 'unknown'): LiteralAnchorRequest => ({
  kind: 'literal',
  alias,
  type_text: text,
  anchor_origin: 'manifest-placeholder',
});

describe('capture: a placeholder literal keeps its cause on the record (carrick#3995)', () => {
  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('a checked and a demoted literal both carry the cause; an uncaused one does not', () => {
    const records = capture('orders', [
      { ...placeholder('Orders_Checked'), unread_cause: 'not_located' },
      { ...placeholder('Orders_Demoted', '   '), unread_cause: 'not_located' },
      placeholder('Orders_Plain'),
    ]);
    const demoted = records.get('Orders_Demoted');
    assert.strictEqual(demoted?.capture_failure_reason, 'empty literal type text');
    assert.strictEqual(demoted?.unread_cause, 'not_located');
    assert.strictEqual(records.get('Orders_Checked')?.unread_cause, 'not_located');
    assert.strictEqual(records.get('Orders_Plain')?.unread_cause, undefined);
  });

  it('the cause does not move the surface hash', () => {
    const withCause = capture('with-cause', [
      { ...placeholder('Orders_Hashed', '{ id: string; note: unknown }'), unread_cause: 'not_located' },
    ]).get('Orders_Hashed');
    const without = capture('without-cause', [
      placeholder('Orders_Hashed', '{ id: string; note: unknown }'),
    ]).get('Orders_Hashed');
    assert.strictEqual(withCause?.unread_cause, 'not_located');
    assert.ok(without?.surface_hash, 'a checked literal is hashed');
    assert.strictEqual(withCause?.surface_hash, without?.surface_hash);
  });

  it('the request validator keeps the cause on a literal anchor', () => {
    const parsed = parseRequest({
      action: 'capture_v2',
      request_id: 'r1',
      repo_root: root,
      service_name: 'orders',
      out_dir: path.join(root, 'validated-stub'),
      anchors: [{ ...placeholder('Orders_Valid'), unread_cause: 'not_located' }],
    });
    assert.strictEqual(parsed.success, true, JSON.stringify(parsed));
    const anchors = (parsed as { request: { anchors: LiteralAnchorRequest[] } }).request.anchors;
    assert.strictEqual(anchors[0].unread_cause, 'not_located');
  });
});
