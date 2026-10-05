/**
 * A capture anchor's `param_name` arrives at the capture when the request
 * comes through the sidecar process (carrick#1980).
 *
 * A subscriber's contract is what its handler receives, so its anchor names a
 * parameter and the capture resolves that parameter before anything else
 * (carrick#498). The scanner never calls the capture in process: it writes a
 * `capture_v2` request to the sidecar's stdin, where the request schema reads
 * it. The schema did not declare `param_name` on an `infer` anchor and drops
 * what it does not declare, so every such anchor lost its parameter name
 * before the capture saw it and fell to the expression locator, which types
 * whatever expression starts on the anchor's line: the callback itself, or a
 * comparison inside the handler. Both self-check clean, and so does the
 * callback of an untyped parameter, which is a function where the parameter
 * is a top type the check would not have judged.
 *
 * Every test that named a parameter called `captureStub` directly, where no
 * schema stands between the request and the capture. This one sends the
 * request the way the Rust client serialises it (the `kind` tag first, the
 * fields in declaration order, an absent option omitted) to a sidecar process
 * and reads what that process wrote.
 *
 * In each fixture the parameter is not the first expression on the anchor's
 * line, so the two readings cannot agree by accident.
 *
 * Fixtures are synthetic and generically named.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import type { CaptureStubResult } from '../src/capture/api.js';

/** A handler handed to its registration as a member of an options object. */
const ORDERS_TS = `export interface OrderEvent {
  orderId: string;
  total: number;
}

interface Bus {
  subscribe(options: {
    subject: string;
    callback: (event: OrderEvent) => Promise<void>;
  }): void;
}

declare const bus: Bus;
declare function record(event: OrderEvent): Promise<void>;

bus.subscribe({
  subject: 'orders',
  callback: async (event) => {
    await record(event);
  },
});

interface UntypedBus {
  subscribe(options: {
    subject: string;
    callback: (event: any) => Promise<void>;
  }): void;
}

declare const untypedBus: UntypedBus;

untypedBus.subscribe({
  subject: 'audit',
  callback: async (entry) => {
    await record(entry);
  },
});
`;

/** A named handler whose body opens with statements of its own. */
const PREVIEW_TS = `export interface PreviewMessage {
  origin: string;
  data?: { type: string };
}

declare const expectedOrigin: string;
declare function setReady(ready: boolean): void;

export function listener(message: PreviewMessage) {
  if (message.origin !== expectedOrigin) {
    return;
  }

  if (message?.data?.type === 'ready') {
    setReady(true);
  }
}
`;

/** 1-based line of the first line of `source` containing `needle`. */
function lineOf(source: string, needle: string): number {
  const index = source.split('\n').findIndex((line) => line.includes(needle));
  assert.ok(index >= 0, `fixture line not found: ${needle}`);
  return index + 1;
}

/** One `CaptureAnchor::Infer` as serde writes it: no span, no text. */
function paramAnchor(alias: string, file: string, line: number, param: string) {
  return {
    kind: 'infer',
    alias,
    source_file: file,
    anchor_origin: 'deterministic-infer',
    line_number: line,
    param_name: param,
  };
}

interface CaptureFrame {
  request_id: string;
  status: string;
  result?: CaptureStubResult;
  errors?: string[];
}

describe('capture_v2 over stdio: an anchor that names a parameter (carrick#1980)', () => {
  let repoDir: string;
  let outRoot: string;
  let result: CaptureStubResult;
  let surface: string;
  const client = new SidecarClient();

  before(async () => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1980-repo-')));
    outRoot = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1980-stub-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          rootDir: 'src',
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'orders.ts'), ORDERS_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'preview.ts'), PREVIEW_TS);

    const comparison = lineOf(PREVIEW_TS, "=== 'ready'");
    await client.start();
    const frame = await client.send<CaptureFrame>(
      {
        action: 'capture_v2',
        request_id: 'capture-1980',
        repo_root: repoDir,
        service_name: 'subscriber-svc',
        anchors: [
          paramAnchor(
            'Callback_Producer_Response',
            'src/orders.ts',
            lineOf(ORDERS_TS, 'callback: async (event) =>'),
            'event'
          ),
          paramAnchor(
            'Untyped_Producer_Response',
            'src/orders.ts',
            lineOf(ORDERS_TS, 'callback: async (entry) =>'),
            'entry'
          ),
          paramAnchor('Body_Producer_Response', 'src/preview.ts', comparison, 'message'),
          paramAnchor('Member_Producer_Response', 'src/preview.ts', comparison, 'message.data'),
          paramAnchor(
            'Signature_Producer_Response',
            'src/preview.ts',
            lineOf(PREVIEW_TS, 'export function listener'),
            'message'
          ),
        ],
        out_dir: path.join(outRoot, 'stub'),
        scan_root: repoDir,
      },
      120_000
    );
    assert.strictEqual(frame.status, 'success', JSON.stringify(frame.errors));
    assert.ok(frame.result, 'the capture answered with no result');
    result = frame.result;
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  const aliasText = (alias: string): string => {
    const match = surface.match(new RegExp(`export type ${alias} = ([^;]+);`));
    assert.ok(match, `no surface line for ${alias} in:\n${surface}`);
    return match[1].trim();
  };
  const recordOf = (alias: string) => {
    const record = result.aliases.find((entry) => entry.alias === alias);
    assert.ok(record, `no record for ${alias}`);
    return record;
  };

  it('a callback in an options object is typed by its parameter, not as the callback', () => {
    const text = aliasText('Callback_Producer_Response');
    assert.match(text, /OrderEvent/);
    assert.doesNotMatch(text, /=>/, `the callback itself was captured: ${text}`);
    assert.strictEqual(recordOf('Callback_Producer_Response').self_check, 'ok');
  });

  it('a callback whose parameter is untyped reads as untyped, not as a typed function', () => {
    // The callback's own type, `(entry: any) => Promise<void>`, is a function
    // and self-checks clean, so it was published and judged. The parameter is
    // a top type, which the self-check records and the check does not judge.
    assert.strictEqual(aliasText('Untyped_Producer_Response'), 'any');
    const record = recordOf('Untyped_Producer_Response');
    assert.strictEqual(record.self_check, 'decayed_internal');
    assert.strictEqual(record.top_type_at_self_check, true);
  });

  it('an anchor on a line inside the handler is typed by the parameter, not by that line', () => {
    const text = aliasText('Body_Producer_Response');
    assert.match(text, /PreviewMessage/);
    assert.notStrictEqual(text, 'boolean');
    assert.strictEqual(recordOf('Body_Producer_Response').self_check, 'ok');
  });

  it('a name no parameter has abstains and says so, rather than typing the line', () => {
    assert.strictEqual(aliasText('Member_Producer_Response'), 'unknown');
    const reason = recordOf('Member_Producer_Response').capture_failure_reason ?? '';
    assert.match(reason, /no handler parameter 'message\.data' resolved/);
  });

  it("an anchor on the handler's own line is typed by the parameter", () => {
    assert.match(aliasText('Signature_Producer_Response'), /PreviewMessage/);
  });
});
