/**
 * Why a bare LLM symbol anchor cannot be trusted to carry array-ness.
 *
 * Live repro (carrick-demo trio, scanner v0.3.7): user-service's
 * `GET /api/users/:id/orders` answers `res.json(userOrders)` where
 * `userOrders` is `(await axios.get<Order[]>(...)).data.filter(...)`. The
 * producer is a correct `Order[]`; the consumer types a correct `Order[]`;
 * the scan reported a CAUTION type_mismatch — "Order missing properties from
 * Order[]: length, pop, push, concat, and 29 more".
 *
 * The two facts behind that, both pinned here:
 *
 *  1. On the bare checkout CI actually scans (#349) the client library is
 *     unresolved, so the payload decays to a BARE `any` and the
 *     `response_body` inference reports no `primary_type_symbol` and no
 *     `array_depth`. That is the only channel through which the use-site's
 *     `[]` can reach the capture — an LLM `primary_type_symbol` is a bare
 *     element identifier by schema contract.
 *
 *  2. A symbol anchor captured with no depth emits the ELEMENT as a confident
 *     surface line. Nothing downstream can tell that apart from a genuinely
 *     scalar contract.
 *
 * Together they mean array-ness was GUESSED, so the scanner-side fix
 * (`derive_capture_anchors`, engine/type_compat_v2.rs) declines to emit the
 * symbol anchor at all when the inference came back blind, leaving the alias
 * to its own infer anchor — which, as pinned below, captures `any` and
 * self-checks `decayed_internal`, routing the pair to unverifiable instead of
 * a false mismatch.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';

const BARE = path.join(FIXTURES_PATH, '..', 'capture-v2-bare');
const SOURCE_REL = 'src/http/decayed-array-response.ts';
const SOURCE = path.join(BARE, SOURCE_REL);

/** Byte span of `text` in the fixture (ASCII-only, so byte == char offsets). */
function spanOf(text: string): { start: number; end: number; line: number } {
  const source = fs.readFileSync(SOURCE, 'utf-8');
  const start = source.indexOf(text);
  assert.ok(start >= 0, `fixture must contain: ${text}`);
  assert.strictEqual(
    source.indexOf(text, start + 1),
    -1,
    `fixture must contain exactly one occurrence of: ${text}`
  );
  return {
    start,
    end: start + text.length,
    line: source.slice(0, start).split('\n').length,
  };
}

interface InferResponseShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    primary_type_symbol?: string;
    array_depth?: number;
  }>;
  errors?: string[];
}

describe('a payload decayed through an unresolved dependency reports no array depth', () => {
  let client: SidecarClient;

  before(async () => {
    assert.ok(
      !fs.existsSync(path.join(BARE, 'node_modules')),
      'precondition: capture-v2-bare must have no node_modules'
    );
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'blind-init', repo_root: BARE });
  });

  after(async () => {
    await client.stop();
  });

  it('response_body inference over an unresolved client resolves to a bare `any`', async () => {
    const payload = spanOf('send(inStock)');
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: 'blind-infer',
      requests: [
        {
          file_path: SOURCE,
          line_number: payload.line,
          span_start: payload.start,
          span_end: payload.end,
          infer_kind: 'response_body',
          alias: 'Endpoint_blind_Response',
        },
      ],
    });

    const inferred = response.inferred_types?.find(
      (t) => t.alias === 'Endpoint_blind_Response'
    );
    assert.ok(
      inferred,
      `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`
    );
    // Bare `any`, not a partially decayed shape: nothing about the use site
    // was seen, which is exactly what the scanner-side guard keys on.
    assert.strictEqual(inferred.type_string.trim(), 'any');
    assert.strictEqual(
      inferred.primary_type_symbol,
      undefined,
      'a decayed payload has no anchor symbol to agree with the LLM symbol'
    );
    assert.strictEqual(
      inferred.array_depth,
      undefined,
      'the `[]` the caller wrote is unrecoverable here — there is no depth to ' +
        'copy onto the explicit SymbolRequest for this alias'
    );
  });

  it('a symbol anchor with no depth emits the bare element as a confident line', () => {
    const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-blind-'));
    const result = captureStub({
      repoRoot: BARE,
      serviceName: 'blind-depth',
      outDir,
      anchors: [
        {
          kind: 'symbol',
          alias: 'Endpoint_no_depth_Response',
          symbol_name: 'Item',
          source_file: 'src/app/models/item.ts',
          anchor_origin: 'llm-symbol',
        },
        {
          kind: 'symbol',
          alias: 'Endpoint_depth_Response',
          symbol_name: 'Item',
          source_file: 'src/app/models/item.ts',
          anchor_origin: 'llm-symbol',
          array_depth: 1,
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));

    const surface = fs.readFileSync(
      path.join(result.stub_dir, 'types', 'surface.d.ts'),
      'utf-8'
    );
    // Depth is the ONLY difference between a correct `Item[]` contract and a
    // wrong `Item` one, and both record self_check 'ok' — the wrong one is
    // indistinguishable downstream, which is why the guard must sit upstream.
    assert.match(surface, /Endpoint_no_depth_Response = import\('[^']+'\)\.Item;/);
    assert.match(surface, /Endpoint_depth_Response = import\('[^']+'\)\.Item\[\];/);
    for (const alias of result.aliases) {
      assert.strictEqual(alias.self_check, 'ok', JSON.stringify(alias));
    }
  });

  it('the infer anchor the guard falls back to captures `any` and self-checks decayed', () => {
    const payload = spanOf('send(inStock)');
    const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-blind-'));
    const result = captureStub({
      repoRoot: BARE,
      serviceName: 'blind-infer-anchor',
      outDir,
      anchors: [
        {
          kind: 'infer',
          alias: 'Endpoint_blind_Response',
          source_file: SOURCE_REL,
          anchor_origin: 'deterministic-infer',
          span_start: payload.start + 'send('.length,
          span_end: payload.end - 1,
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));

    const record = result.aliases.find((a) => a.alias === 'Endpoint_blind_Response');
    assert.ok(record, JSON.stringify(result.aliases));
    assert.strictEqual(record.self_check, 'decayed_internal');
    assert.strictEqual(record.top_type_at_self_check, true);
    // No capture_failure_reason: the literal backfill (`backfill_anchors`,
    // engine/type_compat_v2.rs) keys on that field, so it cannot silently
    // re-anchor this alias with the v1 bundle's bare-element text.
    assert.strictEqual(record.capture_failure_reason, undefined);
  });
});
