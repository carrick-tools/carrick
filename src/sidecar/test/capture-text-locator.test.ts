/**
 * carrick#3500 step 2: the capture's text locator keeps the answers it gave
 * before the inferrer and the capture shared one locator module.
 *
 * The capture binds a locator's text to a node only on an exact match (after
 * the shared whitespace and quote rule), on or after the stated line anywhere
 * in the file, else falls back to the first expression on the line. The
 * inferrer's fuzzier tiers (containment, a ±5 line window, a node the text
 * contains) and its trailing-comma and bracket-space rules each moved published
 * capture answers on the corpus when the capture ran them:
 *
 *  - a body read stated at a handler's opening line but written further down
 *    the handler bound to the whole handler, whose function type was published
 *    as the request body;
 *  - a call's payload written over several lines with trailing commas, stated
 *    compactly, bound to the payload object instead of the call on its line, so
 *    a sender's `void` result was replaced by the payload's type;
 *  - the same rule moved a reply call's payload with a nullable field from the
 *    reply call (refused as transport machinery) to the payload, which the
 *    check then compared.
 *
 * Each case asks `captureStub` and reads the published surface.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

const SOURCE_TS = `interface Incoming<B> { body: B }
declare function guarded(
  fn: (req: Incoming<{ mode: "on" | "off" }>, res: unknown) => Promise<void>
): unknown;
declare function audit(): void;
declare const channel: { send(topic: string, body: unknown): Promise<void> };
declare const ctx: { json<T>(body: T): Response };
declare const when: string | undefined;

export const update = guarded(async (req, res) => {
  audit();
  audit();
  audit();
  audit();
  audit();
  audit();
  audit();
  audit();
  const input = req.body;
  void input;
});

export async function notify(id: string) {
  await channel.send("DONE", {
    id,
    ok: true,
  });
}

export function summary(values: Record<string, number>) {
  return ctx.json({
    values,
    completedAt: when ?? null,
  });
}
`;

const LINES = SOURCE_TS.split('\n');
function lineWith(fragment: string): number {
  const idx = LINES.findIndex((l) => l.includes(fragment));
  assert.ok(idx >= 0, `fixture must contain ${fragment}`);
  return idx + 1;
}

/** Each anchor: the locator text and the line the scanner sends with it. */
const ANCHORS: Record<string, { text: string; line: number }> = {
  // Stated at the handler's opening line; the read is nine lines below.
  bodyRead: { text: 'req.body', line: lineWith('export const update') },
  // The payload as a model prints it: one line, no trailing comma.
  senderPayload: { text: '{ id, ok: true }', line: lineWith('channel.send(') },
  replyPayload: { text: '{ values, completedAt: when ?? null }', line: lineWith('ctx.json(') },
};

describe('carrick#3500: the capture text locator keeps its answers', () => {
  let repoDir: string;
  let outRoot: string;
  let result: CaptureStubResult;
  let surface: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3500-locator-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3500-locator-stub-'));
    fs.mkdirSync(path.join(repoDir, 'src'));
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
    fs.writeFileSync(path.join(repoDir, 'src', 'route.ts'), SOURCE_TS);

    result = captureStub({
      repoRoot: repoDir,
      serviceName: 'locator-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: Object.entries(ANCHORS).map(([name, { text, line }]) => ({
        kind: 'infer' as const,
        alias: `Capture_${name}`,
        source_file: 'src/route.ts',
        anchor_origin: 'deterministic-infer' as const,
        expression_text: text,
        line_number: line,
      })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  function captured(name: string): { text: string; failure: string } {
    const alias = `Capture_${name}`;
    const record = result.aliases.find((a) => a.alias === alias);
    assert.ok(record, `no capture record for ${alias}`);
    const m = surface.match(
      new RegExp(`export type ${alias} = ([\\s\\S]*?);(?=\\s*(?:export type |$))`)
    );
    assert.ok(m, `no surface line for ${alias}:\n${surface}`);
    return {
      text: m![1].replace(/\s+/g, ' ').trim(),
      failure: record!.capture_failure_reason ?? '',
    };
  }

  it('a body read stated at the handler line binds the read below, not the handler', () => {
    assert.strictEqual(captured('bodyRead').text, '{ mode: "on" | "off"; }');
  });

  it("a sender call's multi-line payload keeps the call's void result", () => {
    assert.strictEqual(captured('senderPayload').text, 'void');
  });

  it("a reply call's multi-line payload with a nullable field stays on the reply call", () => {
    const got = captured('replyPayload');
    assert.match(got.failure, /framework machinery/, JSON.stringify(got));
  });
});
