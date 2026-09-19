/**
 * carrick#1046: two captures of ONE tree must not disturb each other.
 *
 * The surface entry has to be written inside the scanned tree (an entry beside
 * a `rootDir` of `src` fails TS6059), and it used to carry one name per repo
 * root. Two captures of the same tree therefore shared a single file: whichever
 * finished first unlinked it while the other's program was still reading it,
 * and every alias whose node-builder print anchors in that destination demoted
 * to `structural_fallback` — honestly, for a reason that described the harness
 * and not the code. The sibling symptom one layer up is `decayed_internal`
 * where `ok` was expected.
 *
 * It surfaced as a flaky merge gate rather than as a bug, because it needs
 * concurrency to appear: each test file passes alone every time, and the four
 * files that capture `capture-v2-bare` only collide when `node --test` runs
 * them together, which is what CI does. Measured on `main` before the fix, that
 * combination went red on 5 of 20 invocations.
 *
 * The assertion here is not a tier by name. It is that a capture made while
 * others run over the same tree is IDENTICAL to one made alone — which is the
 * property the shared name broke, and which cannot rot as the fixture's
 * expected tiers change.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as os from 'node:os';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { surfaceEntryFileName } from '../src/capture/index.js';
import type { CaptureAliasRecord, CaptureStubResult } from '../src/capture/api.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const BARE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'capture-v2-bare');

/**
 * The captures below must share ONE tree — that is the condition under test —
 * but not the tree the other capture suites use: this file asserts what is
 * present in the scanned directory, and a sibling suite's entry file, in flight
 * over the same fixture, would read as a leftover. So the fixture is copied
 * once and every capture here runs against the copy.
 */
const TREE = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-concurrent-tree-'));
// The copy skips entry files, because a sibling suite capturing the original
// may have one IN FLIGHT as this runs — which is the same fact under test: the
// entry lives in the scanned tree, so anything reading that tree has to expect
// another capture's file to be in it.
fs.cpSync(BARE, TREE, {
  recursive: true,
  filter: (src) => !path.basename(src).startsWith('__carrick_surface__'),
});

interface CaptureV2ResponseShape {
  request_id: string;
  status: string;
  result?: CaptureStubResult;
  errors?: string[];
}

const ANCHORS = [
  {
    kind: 'symbol',
    alias: 'Endpoint_order_Response',
    symbol_name: 'OrderResponse',
    source_file: 'src/http/routes.ts',
    anchor_origin: 'llm-symbol',
  },
  {
    kind: 'handler_return',
    alias: 'Endpoint_getorder_Handler',
    symbol_name: 'getOrder',
    source_file: 'src/http/routes.ts',
    anchor_origin: 'llm-symbol',
  },
  {
    kind: 'symbol',
    alias: 'Pub_shipmentthing_Payload',
    symbol_name: 'ShipmentThing',
    source_file: 'src/events/pub.ts',
    anchor_origin: 'llm-symbol',
  },
];

interface CaptureOutcome {
  aliases: CaptureAliasRecord[];
  /** The emitted surface, which is what a disturbed print actually changes. */
  surface: string;
}

/** One capture of the bare fixture over the wire, in its own sidecar process. */
async function capture(label: string): Promise<CaptureOutcome> {
  const client = new SidecarClient();
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), `carrick-concurrent-${label}-`));
  try {
    await client.start();
    const response = (await client.send({
      request_id: `capture-v2-concurrent-${label}`,
      action: 'capture_v2',
      repo_root: TREE,
      service_name: 'Capture Bare Svc',
      out_dir: path.join(outDir, 'stub'),
      anchors: ANCHORS,
    })) as CaptureV2ResponseShape;
    assert.equal(response.status, 'success', JSON.stringify(response.errors));
    const aliases = response.result?.aliases ?? [];
    assert.equal(aliases.length, ANCHORS.length, `${label} answered every anchor`);
    const stubDir = response.result?.stub_dir ?? '';
    return {
      aliases,
      surface: fs.readFileSync(path.join(stubDir, 'types', 'surface.d.ts'), 'utf8'),
    };
  } finally {
    await client.stop();
    fs.rmSync(outDir, { recursive: true, force: true });
  }
}

/** Tier, self-check, failure reason and emitted surface — everything a
 * disturbed capture moves. */
function shape(outcome: CaptureOutcome): string {
  return JSON.stringify([
    [...outcome.aliases]
      .sort((a, b) => a.alias.localeCompare(b.alias))
      .map((entry) => [
        entry.alias,
        entry.serialization,
        entry.self_check,
        entry.capture_failure_reason ?? null,
      ]),
    outcome.surface,
  ]);
}

describe('capture_v2: concurrent captures of one tree (carrick#1046)', () => {
  after(() => fs.rmSync(TREE, { recursive: true, force: true }));

  it('gives every concurrent capture the answer it would get alone', async () => {
    const alone = shape(await capture('alone'));
    const together = await Promise.all(
      ['a', 'b', 'c', 'd'].map((label) => capture(label))
    );
    for (const [index, outcome] of together.entries()) {
      assert.equal(
        shape(outcome),
        alone,
        `concurrent capture ${index} matches the capture made alone`
      );
    }
  });

  it('gives each capture a surface entry name of its own', () => {
    const first = surfaceEntryFileName();
    const second = surfaceEntryFileName();
    assert.notEqual(first, second);
    for (const name of [first, second]) {
      // The prefix is what makes a leftover from an interrupted capture
      // recognisable (carrick#1069); the pid is what separates processes.
      assert.ok(name.startsWith('__carrick_surface__.'), name);
      assert.ok(name.includes(`.${process.pid}.`), name);
    }
  });

  it('leaves no entry file behind in the scanned tree', async () => {
    await capture('cleanup');
    const leftovers = fs
      .readdirSync(path.join(TREE, 'src'))
      .filter((name) => name.startsWith('__carrick_surface__'));
    assert.deepEqual(leftovers, []);
  });
});
