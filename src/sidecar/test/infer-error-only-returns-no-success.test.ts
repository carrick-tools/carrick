/**
 * carrick#4148: a handler whose every return states an error or redirect
 * status has no success body. Its response is `unknown` with the decided
 * reason `no_success_payload`, not a blind `unknown`. A success branch still
 * publishes its body; a return that states no status stays without a reason.
 * `replykit` is a synthetic stand-in: no framework name is read.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const KIT_DTS = `export declare function json(body: unknown, status: number): Response;
export declare function redirect(url: string, status: number): Response;
export declare function opaque(input: string): Response;
`;

const ROUTE_TS = `import { json, redirect, opaque } from 'replykit';

declare function check(token: string): Promise<{ ok: boolean; url: string }>;

export async function errorsOnly(token: string) {
  const result = await check(token);
  if (!result.ok) {
    return json({ error: 'denied' }, 401);
  }
  return redirect(result.url, 302);
}

export async function withSuccess(token: string) {
  const result = await check(token);
  if (!result.ok) {
    return json({ error: 'denied' }, 401);
  }
  return json({ found: true }, 200);
}

export async function noStatus(token: string) {
  const result = await check(token);
  if (!result.ok) {
    return json({ error: 'denied' }, 401);
  }
  return opaque(result.url);
}
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ reason: string }>;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

function lineOf(marker: string): number {
  const index = ROUTE_TS.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
  return index + 1;
}

describe('carrick#4148: error and redirect returns decide no_success_payload', () => {
  let client: SidecarClient;
  let repoDir: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4148-'));
    const kitDir = path.join(repoDir, 'node_modules', 'replykit');
    fs.mkdirSync(kitDir, { recursive: true });
    fs.writeFileSync(
      path.join(kitDir, 'package.json'),
      JSON.stringify({ name: 'replykit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(kitDir, 'index.d.ts'), KIT_DTS);
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
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(repoDir, 'src', 'route.ts'), ROUTE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function response(marker: string) {
    const alias = `Alias_${marker.replace(/\W+/g, '_')}`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: path.join(repoDir, 'src', 'route.ts'),
          line_number: lineOf(marker),
          infer_kind: 'function_return',
          alias,
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }
  const reasons = (got?: { any_provenance?: Array<{ reason: string }> }): string[] =>
    (got?.any_provenance ?? []).map((p) => p.reason);

  it('error and redirect returns give unknown with no_success_payload', async () => {
    const got = await response('export async function errorsOnly');
    assert.ok(got, 'an entry is published');
    assert.strictEqual(collapse(got.type_string), 'unknown');
    assert.deepStrictEqual(reasons(got), ['no_success_payload']);
  });

  it('a success branch still publishes its body', async () => {
    const got = await response('export async function withSuccess');
    assert.ok(got);
    assert.ok(/found/.test(collapse(got.type_string)), collapse(got.type_string));
    assert.ok(!reasons(got).includes('no_success_payload'), JSON.stringify(got));
  });

  it('a return with no stated status stays without the reason', async () => {
    const got = await response('export async function noStatus');
    assert.ok(!reasons(got).includes('no_success_payload'), JSON.stringify(got));
  });
});
