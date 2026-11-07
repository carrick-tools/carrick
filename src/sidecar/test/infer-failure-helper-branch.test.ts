/**
 * carrick#3523: a route that answers its failure through a helper the repo
 * declares (`return failureHelper(result)`, where every return of the helper is
 * `json({ error }, { status: 4xx|5xx })`) published the helper's ARGUMENT as a
 * member of its success contract. The call states no status itself, so the
 * response-helper walk read the failure object handed to it as the payload,
 * and a consumer that parses only 2xx bodies was judged against it.
 *
 * A call to a repo function whose every return is a send stating an error or
 * redirect status is an error branch, as the same send written inline is. A
 * helper whose status the source does not fix, or that also returns a success
 * send, keeps today's reading. `loaderkit` is a synthetic stand-in: no
 * framework name is read.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const KIT_DTS = `export type TypedResponse<T> = Response & { json(): Promise<T> };
export declare function json<Data>(data: Data, init?: number | ResponseInit): TypedResponse<Data>;
`;

const STORE_TS = `import { json } from 'loaderkit';

export type Failure = { success: false; error: string; status?: 400 | 500 };
export type Signed = { success: true; url: string } | Failure;
export declare function sign(name: string): Promise<Signed>;
declare const code: number;

export function failureResponse(failure: Failure) {
  const status = failure.status ?? 500;
  if (status === 400) {
    return json({ error: failure.error }, { status: 400 });
  }
  return json({ error: 'failed: ' + failure.error }, { status: 500 });
}

export function unfixedResponse(failure: Failure) {
  return json({ error: failure.error }, { status: code });
}

export function eitherResponse(failure: Failure) {
  if (failure.status === 400) {
    return json({ error: failure.error }, { status: 400 });
  }
  return json({ retried: true }, { status: 200 });
}
`;

const BUILDER_TS = `export declare function createLoaderRoute<R>(
  options: { allowJWT: boolean },
  handler: (args: { params: Record<string, string> }) => Promise<R>
): (args: { request: Request }) => Promise<R>;
`;

const ROUTE_TS = `import { json } from 'loaderkit';
import { createLoaderRoute } from './builder';
import { sign, failureResponse, unfixedResponse, eitherResponse } from './store';

export const loader = createLoaderRoute(
  { allowJWT: true },
  async ({ params }) => {
    const signed = await sign(params['*']);
    if (!signed.success) {
      return failureResponse(signed);
    }
    return json({ presignedUrl: signed.url });
  }
);

export async function unfixed({ params }: { params: Record<string, string> }) {
  const signed = await sign(params['*']);
  if (!signed.success) {
    return unfixedResponse(signed);
  }
  return json({ presignedUrl: signed.url });
}

export async function either({ params }: { params: Record<string, string> }) {
  const signed = await sign(params['*']);
  if (!signed.success) {
    return eitherResponse(signed);
  }
  return json({ presignedUrl: signed.url });
}
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

function lineOf(source: string, marker: string): number {
  const index = source.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
  return index + 1;
}

describe('carrick#3523: a failure helper the repo declares is an error branch', () => {
  let client: SidecarClient;
  let repoDir: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3523-'));
    const kitDir = path.join(repoDir, 'node_modules', 'loaderkit');
    fs.mkdirSync(kitDir, { recursive: true });
    fs.writeFileSync(
      path.join(kitDir, 'package.json'),
      JSON.stringify({ name: 'loaderkit', version: '1.0.0', types: 'index.d.ts' })
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
    fs.writeFileSync(path.join(repoDir, 'src', 'store.ts'), STORE_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'builder.ts'), BUILDER_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'route.ts'), ROUTE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function response(marker: string): Promise<string | undefined> {
    const alias = `Alias_${marker.replace(/\W+/g, '_')}`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: path.join(repoDir, 'src', 'route.ts'),
          line_number: lineOf(ROUTE_TS, marker),
          infer_kind: 'function_return',
          alias,
        },
      ],
    });
    const found = (res.inferred_types ?? []).find((t) => t.alias === alias);
    return found ? collapse(found.type_string) : undefined;
  }

  it('publishes only the success body when every return of the helper states an error status', async () => {
    assert.strictEqual(await response('export const loader'), '{ presignedUrl: string; }');
  });

  it('keeps the helper argument when the helper sends a status the source does not fix', async () => {
    const published = await response('export async function unfixed');
    assert.ok(published?.includes('presignedUrl'), `got: ${published}`);
    assert.ok(published?.includes('success: false'), `got: ${published}`);
  });

  it('does not drop the branch when the helper also sends a success status', async () => {
    // Only "not dropped" is pinned: the right reading of this branch is the
    // helper's success return, not its argument (carrick#3530).
    const published = await response('export async function either');
    assert.ok(published?.includes('presignedUrl'), `got: ${published}`);
    assert.notStrictEqual(published, '{ presignedUrl: string; }', 'the branch must not be dropped');
  });
});
