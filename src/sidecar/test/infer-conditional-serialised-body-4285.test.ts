/**
 * carrick#4285: a request body written as a conditional over the serialiser
 * (`body: x ? JSON.stringify(x) : null`) is read through `JSON.stringify` in
 * each arm, the way a direct `body: JSON.stringify(x)` already is. Each arm is
 * read as it is: the serialised arm as its argument's type, any other arm as
 * its own type. The conditional's own `string | null` is the serialised text,
 * never a contract.
 *
 * Both readers answer it: the compiler engine's infer (request_body locator)
 * and the capture's infer anchor.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { captureStub } from '../src/capture/index.js';

const CLIENT_TS = `export interface Payload { name: string }
export interface Other { id: number }
declare function send(url: string, init: { method: string; body: string | null }): Promise<unknown>;

export function maybe(payload: Payload | undefined) {
  return send('/m', { method: 'POST', body: payload ? JSON.stringify(payload) : null });
}

export function either(a: Payload, b: Other, pick: boolean) {
  return send('/e', { method: 'POST', body: pick ? JSON.stringify(a) : JSON.stringify(b) });
}

export function plain(text: string, pick: boolean) {
  return send('/p', { method: 'POST', body: pick ? text : null });
}

export function direct(single: Payload) {
  return send('/d', { method: 'POST', body: JSON.stringify(single) });
}
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();
const spanOf = (text: string): { start: number; end: number } => {
  const start = CLIENT_TS.indexOf(text);
  assert.ok(start >= 0, `fixture holds no ${JSON.stringify(text)}`);
  assert.strictEqual(CLIENT_TS.indexOf(text, start + 1), -1, `${JSON.stringify(text)} is not unique`);
  return { start, end: start + text.length };
};
const lineAt = (offset: number): number => CLIENT_TS.slice(0, offset).split('\n').length;

const PAYLOAD = String.raw`(\{ name: string; \}|(import\("[^"]*"\)\.)?Payload)`;
const OTHER = String.raw`(\{ id: number; \}|(import\("[^"]*"\)\.)?Other)`;
const CASES: Array<{ alias: string; expr: string; want: RegExp }> = [
  { alias: 'Maybe', expr: 'payload ? JSON.stringify(payload) : null', want: new RegExp(`^(${PAYLOAD} \\| null|null \\| ${PAYLOAD})$`) },
  { alias: 'Either', expr: 'pick ? JSON.stringify(a) : JSON.stringify(b)', want: new RegExp(`^(${PAYLOAD} \\| ${OTHER}|${OTHER} \\| ${PAYLOAD})$`) },
  { alias: 'Plain', expr: 'pick ? text : null', want: /^string \| null$/ },
  { alias: 'Direct', expr: 'JSON.stringify(single)', want: new RegExp(`^${PAYLOAD}$`) },
];

describe('carrick#4285: a conditional serialised request body reads each arm', () => {
  let client: SidecarClient;
  let repoDir: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4285-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', skipLibCheck: true },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'client.ts'), CLIENT_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-4285', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  for (const { alias, expr, want } of CASES) {
    it(`infer reads ${alias}`, async () => {
      const at = spanOf(expr);
      const response = await client.send<InferShape>({
        action: 'infer',
        request_id: `infer-${alias}`,
        requests: [
          {
            file_path: path.join(repoDir, 'src', 'client.ts'),
            line_number: lineAt(at.start),
            span_start: at.start,
            span_end: at.end,
            infer_kind: 'request_body',
            alias,
          },
        ],
      });
      const got = response.inferred_types?.find((t) => t.alias === alias);
      assert.ok(got, `no answer for ${alias}: ${JSON.stringify(response.errors)}`);
      assert.match(collapse(got.type_string), want, got.type_string);
    });

    it(`capture reads ${alias}`, () => {
      const at = spanOf(expr);
      const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4285-out-'));
      try {
        const result = captureStub({
          repoRoot: repoDir,
          serviceName: 'cond-body',
          outDir,
          anchors: [
            {
              kind: 'infer',
              alias: `Endpoint_${alias}_Request`,
              source_file: 'src/client.ts',
              anchor_origin: 'deterministic-infer',
              line_number: lineAt(at.start),
              span_start: at.start,
              span_end: at.end,
              expression_text: expr,
            },
          ],
        });
        assert.ok(result.success, JSON.stringify(result.errors));
        const surface = fs.readFileSync(path.join(outDir, 'types/surface.d.ts'), 'utf-8');
        const start = surface.indexOf(`export type Endpoint_${alias}_Request =`);
        assert.ok(start >= 0, surface);
        const text = collapse(surface.slice(start).split(/export type /)[1] ?? '')
          .replace(/^Endpoint_\w+ = /, '')
          .replace(/;$/, '');
        assert.match(text, want, text);
      } finally {
        fs.rmSync(outDir, { recursive: true, force: true });
      }
    });
  }
});
