/**
 * The capture must abstain when its infer anchor resolves a bare top type
 * (carrick#766).
 *
 * `finishInferAnchor` reads the type at the located node, tries the #433
 * inline-literal recovery when that type is a whole top type, and then — until
 * this guard — printed whatever it held through the node builder. When the
 * recovery could not help, what it printed was `any`, and the alias's surface
 * line read `export type <alias> = any;`.
 *
 * That line is the contract the index publishes. `any` there says "a type was
 * inferred and it collapsed"; the truth is "the locator pointed at a node the
 * checker could tell us nothing about". `unknown` — no contract stated here —
 * is the honest word, and the reason travels on `capture_failure_reason`.
 *
 * Live shape behind this: a route whose handlers are built by a framework
 * factory and re-exported at the bottom of the file. Both operations anchor at
 * the export line; carrick#771 stopped the v1 walk answering with the
 * neighbouring helper, so the alias falls to its own capture infer anchor,
 * which resolved the export statement — type `any` — and published it.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import { FIXTURES_PATH } from './helpers.js';
import type { CaptureAliasRecord } from '../src/capture/api.js';

const BARE = path.join(FIXTURES_PATH, '..', 'capture-v2-bare');
const SOURCE_REL = 'src/http/reexported-binding-route.ts';
const SOURCE = path.join(BARE, SOURCE_REL);

const DECAYED_ALIAS = 'Endpoint_reexported_Response';
const CLEAN_ALIAS = 'Endpoint_clean_Response';
const LOCATED_ALIAS = 'Endpoint_located_Response';

/** Byte span and 1-based line of the sole occurrence of `text` in the fixture. */
function spanOf(text: string): { start: number; end: number; line: number } {
  const source = fs.readFileSync(SOURCE, 'utf-8');
  const at = source.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  assert.strictEqual(
    source.indexOf(text, at + 1),
    -1,
    `fixture must contain exactly one occurrence of: ${text}`
  );
  return {
    start: at,
    end: at + text.length,
    line: source.slice(0, at).split('\n').length,
  };
}

/** 1-based line of the sole occurrence of `text` in the fixture. */
function lineOf(text: string): number {
  return spanOf(text).line;
}

interface Captured {
  records: Map<string, CaptureAliasRecord>;
  surface: string;
}

/** Capture both anchors over the bare fixture and read the emitted surface. */
function capture(): Captured {
  const locatedPayload = spanOf('respond(summary)');
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-toptype-'));
  const result = captureStub({
    repoRoot: BARE,
    serviceName: 'top-type-abstain',
    outDir,
    anchors: [
      {
        kind: 'infer',
        alias: DECAYED_ALIAS,
        source_file: SOURCE_REL,
        anchor_origin: 'deterministic-infer',
        line_number: lineOf('export { action, loader };'),
      },
      {
        kind: 'infer',
        alias: CLEAN_ALIAS,
        source_file: SOURCE_REL,
        anchor_origin: 'deterministic-infer',
        line_number: lineOf('export const acceptedEnvelope'),
        expression_text: "{ batchId: 'batch_1' }",
      },
      {
        kind: 'infer',
        alias: LOCATED_ALIAS,
        source_file: SOURCE_REL,
        anchor_origin: 'deterministic-infer',
        line_number: locatedPayload.line,
        // The argument, not the call: `respond(...)` returns void.
        span_start: locatedPayload.start + 'respond('.length,
        span_end: locatedPayload.end - 1,
      },
    ],
  });
  assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
  return {
    records: new Map(result.aliases.map((r) => [r.alias, r])),
    surface: fs.readFileSync(path.join(outDir, 'types/surface.d.ts'), 'utf-8'),
  };
}

/** True when the surface declares `alias` as exactly `text`, on one line. */
function surfaceDeclares(surface: string, alias: string, text: string): boolean {
  return surface
    .split('\n')
    .some((l) => l.trim() === `export type ${alias} = ${text};`);
}

describe('an infer anchor that resolves a bare top type abstains (#766)', () => {
  const captured = capture();

  it('is a fixture whose anchor really does resolve to nothing', () => {
    // The guard on the guard: if the fixture ever starts resolving, the
    // assertions below would pass for the wrong reason.
    const record = captured.records.get(DECAYED_ALIAS);
    assert.ok(record, 'no record for the decayed alias');
    assert.strictEqual(
      record.top_type_at_self_check,
      true,
      'the fixture anchor must resolve to a top type for this test to mean anything'
    );
  });

  it('publishes `unknown`, never `any`, for the decayed anchor', () => {
    assert.ok(
      !surfaceDeclares(captured.surface, DECAYED_ALIAS, 'any'),
      `a top type must never be published as a contract:\n${captured.surface}`
    );
    assert.ok(
      surfaceDeclares(captured.surface, DECAYED_ALIAS, 'unknown'),
      `expected an honest \`unknown\` for ${DECAYED_ALIAS}:\n${captured.surface}`
    );
  });

  it('does not look like a demotion the scanner can backfill', () => {
    // `backfill_anchors` (engine/type_compat_v2.rs) re-anchors any alias
    // carrying a `capture_failure_reason` off the v1 bundle's text. When the
    // v1 answer for this alias was itself blind, that text is a bare element
    // symbol, and re-anchoring publishes a confident contract whose array-ness
    // was guessed (#349/#306). An abstain has nothing better to fall back to,
    // so it must not present itself as backfillable.
    const record = captured.records.get(DECAYED_ALIAS);
    assert.ok(record, 'no record for the decayed alias');
    assert.strictEqual(record.capture_failure_reason, undefined);
  });

  it('states why, naming what the locator landed on', () => {
    const reason = captured.records.get(DECAYED_ALIAS)?.self_check_detail;
    assert.ok(reason, 'an abstaining alias must carry its reason');
    assert.match(
      reason,
      /bare top type/,
      `the reason must say the type was a bare top type: ${reason}`
    );
    // The reader needs to know WHERE, not just that something failed. On this
    // shape the locator lands on the export specifier itself, so the reason
    // names the re-exported binding and its line.
    assert.match(
      reason,
      /Identifier at src\/http\/reexported-binding-route\.ts:\d+ \(`action`\)/,
      `the reason must name the node the locator resolved: ${reason}`
    );
    // Repo-relative, never the scanner's checkout directory.
    assert.ok(
      !reason.includes(BARE),
      `the reason must not carry an absolute path: ${reason}`
    );
  });

  it('never answers with the neighbouring declaration', () => {
    // carrick#771's assertion, restated on the capture side: whatever this
    // alias reports, it must not be the private helper's shape.
    assert.ok(
      !captured.surface.includes('statuses'),
      `the helper's shape must never be published as the route's contract:\n${captured.surface}`
    );
  });

  it('keeps publishing a top type the anchor NAMED', () => {
    // The narrowing, pinned. A span/expression/parameter anchor is the scanner
    // stating which value is the payload, so the type at it is a fact about
    // that payload even when it decayed to a whole `any` through a missing
    // dependency — the deep-any walk, the check's IsAny gate and the literal
    // backfill (which keys on `capture_failure_reason`) are all built on that
    // being published. Widening #766's abstain to cover these would re-anchor
    // them off the v1 bundle text and guess what the capture could not see.
    const record = captured.records.get(LOCATED_ALIAS);
    assert.ok(record, 'no record for the located alias');
    assert.strictEqual(record.top_type_at_self_check, true);
    assert.strictEqual(
      record.capture_failure_reason,
      undefined,
      'a named payload that decayed is not a demotion'
    );
    assert.ok(
      surfaceDeclares(captured.surface, LOCATED_ALIAS, 'any'),
      `a located payload keeps its decayed print:\n${captured.surface}`
    );
  });

  it('leaves an anchor that resolves a real shape alone', () => {
    const record = captured.records.get(CLEAN_ALIAS);
    assert.ok(record, 'no record for the clean alias');
    assert.strictEqual(
      record.capture_failure_reason,
      undefined,
      'a resolved anchor must not be demoted'
    );
    assert.strictEqual(record.top_type_at_self_check, false);
    assert.match(
      captured.surface,
      /batchId/,
      'the clean control must keep its captured shape'
    );
  });
});
