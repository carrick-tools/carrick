/**
 * v2 check core — corpus end-to-end over the JSON-over-stdio wire.
 *
 * Drives the sidecar exactly as the Rust client will (WP3): capture_v2 to build
 * stub packages for real corpus services, then check_v2 to verify matched
 * pairs, handling the async install protocol's `status: 'progress'` keepalive
 * frames. Uses one corpus-2 pair and one corpus-3 pair (acceptance: two corpus
 * fixtures), both deliberately-incompatible edges from the fixtures, and pins
 * that the full verdict payload is byte-stable across two independent runs.
 *
 * All four fixtures are bare checkouts with no external-dep references in the
 * captured closures, so the check install is local-only (network-free).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import type { CheckResult } from '../src/capture/api.js';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// dist/test -> repo root is four levels up.
const FIXTURES = path.join(__dirname, '..', '..', '..', '..', 'tests', 'fixtures');

interface CaptureResp {
  status: string;
  result?: { success: boolean; stub_dir: string };
  errors?: string[];
}
interface CheckResp {
  status: string;
  result?: CheckResult;
  errors?: string[];
}

describe('check_v2 corpus end-to-end (stdio, byte-stable)', () => {
  const client = new SidecarClient();
  let workRoot: string;
  let stubDirs: Record<string, string>;

  const services: Record<string, { repo: string; symbol: string; file: string }> = {
    'orders-engine': {
      repo: path.join(FIXTURES, 'xrepo-corpus-2', 'orders-engine'),
      symbol: 'OrderPlaced',
      file: 'src/types/order.ts',
    },
    'billing-svc': {
      repo: path.join(FIXTURES, 'xrepo-corpus-2', 'billing-svc'),
      symbol: 'OrderPlaced',
      file: 'src/types/billing.ts',
    },
    'orders-api': {
      repo: path.join(FIXTURES, 'xrepo-corpus-3', 'orders-api'),
      symbol: 'DispatchRequest',
      file: 'src/types/orders.ts',
    },
    'fulfillment-worker': {
      repo: path.join(FIXTURES, 'xrepo-corpus-3', 'fulfillment-worker'),
      symbol: 'DispatchJob',
      file: 'src/types/fulfillment.ts',
    },
  };

  const pairs = [
    {
      pair_key: 'corpus2:order.placed',
      protocol: 'http' as const,
      type_kind: 'response' as const,
      producer: { service_name: 'orders-engine', alias: 'Sig_orders_engine' },
      consumer: { service_name: 'billing-svc', alias: 'Sig_billing_svc' },
    },
    {
      pair_key: 'corpus3:dispatch',
      protocol: 'http' as const,
      type_kind: 'response' as const,
      producer: { service_name: 'orders-api', alias: 'Sig_orders_api' },
      consumer: { service_name: 'fulfillment-worker', alias: 'Sig_fulfillment_worker' },
    },
  ];

  before(async () => {
    workRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-corpus-'));
    await client.start();
    stubDirs = {};
    let n = 0;
    for (const [service, def] of Object.entries(services)) {
      assert.ok(fs.existsSync(def.repo), `fixture missing: ${def.repo}`);
      const outDir = path.join(workRoot, 'stubs', service);
      const resp = await client.send<CaptureResp>(
        {
          request_id: `cap-${n++}`,
          action: 'capture_v2',
          repo_root: def.repo,
          service_name: service,
          out_dir: outDir,
          anchors: [
            {
              kind: 'symbol',
              alias: `Sig_${service.replace(/-/g, '_')}`,
              symbol_name: def.symbol,
              source_file: def.file,
              anchor_origin: 'llm-symbol',
            },
          ],
        },
        120000
      );
      assert.strictEqual(resp.status, 'success', `${service}: ${JSON.stringify(resp.errors)}`);
      stubDirs[service] = resp.result!.stub_dir;
    }
  });

  after(async () => {
    await client.stop();
    fs.rmSync(workRoot, { recursive: true, force: true });
  });

  function checkRequest(id: string) {
    return {
      request_id: id,
      action: 'check_v2' as const,
      stubs: Object.entries(services).map(([service]) => ({
        service_name: service,
        stub_dir: stubDirs[service],
      })),
      pairs,
    };
  }

  it('produces a verdict per pair over the wire (async install protocol)', async () => {
    const resp = await client.send<CheckResp>(checkRequest('chk-1'), 180000);
    assert.strictEqual(resp.status, 'success', JSON.stringify(resp.errors));
    const result = resp.result!;
    assert.strictEqual(result.isolation, 'pnpm');
    assert.strictEqual(result.install_ok, true);
    assert.strictEqual(result.verdicts.length, pairs.length);
    for (const v of result.verdicts) {
      assert.ok(
        ['compatible', 'incompatible', 'unverifiable', 'gate_caught_baked_any'].includes(v.bucket),
        v.bucket
      );
    }
  });

  it('resolves the deliberate corpus incompatibilities', async () => {
    const resp = await client.send<CheckResp>(checkRequest('chk-2'), 180000);
    const byKey = new Map(resp.result!.verdicts.map((v) => [v.pair_key, v]));
    // corpus-2: OrderPlaced.total Money(object) vs bare number.
    assert.strictEqual(byKey.get('corpus2:order.placed')!.bucket, 'incompatible');
    // corpus-3: DispatchRequest.dispatchAfter string vs DispatchJob Date.
    assert.strictEqual(byKey.get('corpus3:dispatch')!.bucket, 'incompatible');
  });

  it('verdicts are byte-stable across two independent check runs', async () => {
    const a = await client.send<CheckResp>(checkRequest('chk-a'), 180000);
    const b = await client.send<CheckResp>(checkRequest('chk-b'), 180000);
    assert.strictEqual(
      JSON.stringify(a.result!.verdicts),
      JSON.stringify(b.result!.verdicts)
    );
  });

  it('no verdict diagnostic leaks an absolute path or scan internal', async () => {
    const resp = await client.send<CheckResp>(checkRequest('chk-3'), 180000);
    for (const v of resp.result!.verdicts) {
      if (!v.diagnostic) continue;
      assert.ok(!/\/(private\/)?tmp\//.test(v.diagnostic), v.diagnostic);
      assert.ok(!v.diagnostic.includes(workRoot), v.diagnostic);
    }
  });
});
