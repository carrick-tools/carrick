/**
 * carrick#3500 step 2: the inferrer and the capture find a site's node and
 * decide whether its type is framework machinery with ONE answer
 * (`capture/site-node.ts`).
 *
 * Before, each kept its own copy of both, and the copies disagreed:
 *
 *  - the node locator: the inferrer reads a string literal the same in any
 *    quotes, the capture did not, so a locator written in other quotes than
 *    the source located the payload in one and a different node in the other;
 *  - the machinery test: only the capture descended a function value's
 *    awaited return, and only the capture read a property no declaration
 *    states (a mapped `Record<'response', Response>`), so the inferrer
 *    published a transport type the capture refused.
 *
 * Every case is asked through the public entry points (`infer` and
 * `captureStub`) on one fixture, so each assertion holds only while both
 * callers use the shared module. Where the two copies already agreed
 * (carrick#447's four nested shapes, a plain payload), the test pins that they
 * still agree.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';
import { SidecarClient } from './helpers.js';

const SOURCE_TS = `export interface Payload { id: number }
export interface MyResp extends Response { extra: string }
declare const r: Response;
declare const rec: Record<'response', Response>;
declare const mine: MyResp;
declare function send(m: unknown, b: unknown): void;
declare const meta: unknown;
export function direct() { return r; }
export function envelope() { return { response: r }; }
export function wrapperFn() { return () => fetch("/x"); }
export function mapped() { return rec; }
export function deepEnvelope() { return { outer: { response: r } }; }
export function promiseProp() { return { response: fetch("/x") }; }
export function arrayOf() { return [r]; }
export function extendsResponse() { return mine; }
export function plain() { return { id: 1 } as Payload; }
export function quoted() { send(meta, { id: "a" }); }
`;

const LINES = SOURCE_TS.split('\n');
function lineOf(name: string): number {
  const idx = LINES.findIndex((l) => l.includes(`function ${name}(`));
  assert.ok(idx >= 0, `fixture must declare ${name}`);
  return idx + 1;
}

/** Each site: the function it sits in and the locator text the scanner sends. */
const SITES: Record<string, string> = {
  direct: 'r',
  envelope: '{ response: r }',
  wrapperFn: '() => fetch("/x")',
  mapped: 'rec',
  deepEnvelope: '{ outer: { response: r } }',
  promiseProp: '{ response: fetch("/x") }',
  arrayOf: '[r]',
  extendsResponse: 'mine',
  plain: '{ id: 1 } as Payload',
  // The source writes "a"; the locator writes 'a'.
  quoted: "{ id: 'a' }",
};

interface Inferred {
  alias: string;
  type_string: string;
}

describe('carrick#3500: one node locator and one machinery test for infer and capture', () => {
  let repoDir: string;
  let outRoot: string;
  let responseBody: Map<string, string>;
  let expression: Map<string, string>;
  let result: CaptureStubResult;
  let surface: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3500-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3500-stub-'));
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
    const file = path.join(repoDir, 'src', 'route.ts');
    fs.writeFileSync(file, SOURCE_TS);

    const client = new SidecarClient();
    await client.start();
    try {
      await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
      const requests = Object.entries(SITES).flatMap(([name, text]) => {
        const line = lineOf(name);
        const base = { file_path: file, line_number: line, expression_text: text, expression_line: line };
        return [
          { ...base, infer_kind: 'response_body', alias: `Body_${name}` },
          { ...base, infer_kind: 'expression', alias: `Expr_${name}` },
        ];
      });
      const res = await client.send<{ inferred_types?: Inferred[] }>({
        action: 'infer',
        request_id: 'sites',
        requests,
      });
      const byAlias = new Map((res.inferred_types ?? []).map((t) => [t.alias, t.type_string]));
      responseBody = new Map(Object.keys(SITES).flatMap((n) => {
        const t = byAlias.get(`Body_${n}`);
        return t === undefined ? [] : [[n, t] as [string, string]];
      }));
      expression = new Map(Object.keys(SITES).flatMap((n) => {
        const t = byAlias.get(`Expr_${n}`);
        return t === undefined ? [] : [[n, t] as [string, string]];
      }));
    } finally {
      await client.stop();
    }

    result = captureStub({
      repoRoot: repoDir,
      serviceName: 'site-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: Object.entries(SITES).map(([name, text]) => ({
        kind: 'infer' as const,
        alias: `Capture_${name}`,
        source_file: 'src/route.ts',
        anchor_origin: 'deterministic-infer' as const,
        expression_text: text,
        line_number: lineOf(name),
        unwrap: 'none' as const,
      })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  function captured(name: string): { text: string; machinery: boolean } {
    const alias = `Capture_${name}`;
    const record = result.aliases.find((a) => a.alias === alias);
    assert.ok(record, `no capture record for ${alias}`);
    const m = surface.match(
      new RegExp(`export type ${alias} = ([\\s\\S]*?);(?=\\s*(?:export type |$))`)
    );
    assert.ok(m, `no surface line for ${alias}:\n${surface}`);
    return {
      text: m![1].replace(/\s+/g, ' ').trim(),
      machinery: /framework machinery/.test(record!.capture_failure_reason ?? ''),
    };
  }

  /** The inferrer's machinery answer: the response-body read abstains on it. */
  const inferrerSaysMachinery = (name: string): boolean => !responseBody.has(name);

  it('a locator in other quotes than the source locates the same node in both', () => {
    assert.strictEqual(expression.get('quoted'), '{ id: string; }');
    assert.strictEqual(captured('quoted').text, '{ id: string; }');
  });

  for (const name of ['wrapperFn', 'mapped']) {
    it(`both refuse a transport type the capture alone refused before (${name})`, () => {
      assert.ok(captured(name).machinery, `capture must demote ${name} as machinery`);
      assert.ok(
        inferrerSaysMachinery(name),
        `infer must abstain on ${name}, got: ${responseBody.get(name)}`
      );
    });
  }

  for (const name of ['direct', 'envelope']) {
    it(`both still refuse the machinery they agreed on (${name})`, () => {
      assert.ok(captured(name).machinery);
      assert.ok(inferrerSaysMachinery(name));
    });
  }

  // carrick#447: neither copy reaches these shapes. They stay as they were
  // here; the point pinned is that the two answers agree.
  for (const name of ['deepEnvelope', 'promiseProp', 'arrayOf', 'extendsResponse', 'plain']) {
    it(`both answer the same where the copies agreed (${name})`, () => {
      assert.strictEqual(
        captured(name).machinery,
        inferrerSaysMachinery(name),
        `capture ${JSON.stringify(captured(name))} vs infer ${responseBody.get(name)}`
      );
    });
  }
});
