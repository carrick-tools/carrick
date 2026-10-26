/**
 * carrick#807 residual: a file route exported as a call to a route builder
 * (`export const GET = builder(options, async (...) => { ... })`) is asked as
 * `function_return` at the export line. No function starts within two lines of
 * that line (the handler follows the options object), so the request answered
 * nothing, while the same line asked as `response_body` follows the
 * registration to its handler. `function_return` follows it too.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVICE_TS = `type TypedResponse<T> = { readonly body: T };
declare function json<T>(data: T, init?: ResponseInit): TypedResponse<T>;
declare function createRoute<P>(
  opts: { params: P },
  handler: (a: { params: P }) => Promise<unknown>
): (...args: unknown[]) => Promise<unknown>;

const schema = {} as { id: string };

export const GET = createRoute(
  {
    params: schema,
  },
  async ({ params }) => {
    const total = 1;
    return json({ id: params.id, total });
  }
);

export const MISSING = createRoute(
  {
    params: schema,
  },
  async () => {
    return json({ error: 'x' }, { status: 404 });
  }
);

export async function PLAIN() {
  return json({ ok: true });
}
`;

const lineOf = (text: string): number => {
  const at = SERVICE_TS.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return SERVICE_TS.slice(0, at).split('\n').length;
};

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#807: function_return at a builder-call export follows the handler', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-807-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['es2022', 'dom'] },
        include: ['src'],
      })
    );
    servicePath = path.join(repoDir, 'src', 'route.ts');
    fs.writeFileSync(servicePath, SERVICE_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-807', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (kind: string, alias: string, at: string): Promise<string | undefined> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}-${kind}`,
      requests: [{ file_path: servicePath, line_number: lineOf(at), infer_kind: kind, alias }],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    return inferred ? collapse(inferred.type_string) : undefined;
  };

  it('reads the handler the builder call hands in', async () => {
    const text = await infer('function_return', 'Get', 'export const GET');
    assert.ok(text && /id: string/.test(text) && /total: number/.test(text), `expected the handler body, got: ${text}`);
  });

  it('answers a function_return exactly as a response_body at the same line', async () => {
    for (const [alias, at] of [['Get2', 'export const GET'], ['Missing', 'export const MISSING']] as const) {
      const viaReturn = await infer('function_return', alias, at);
      const viaBody = await infer('response_body', alias, at);
      assert.strictEqual(viaReturn, viaBody, `function_return and response_body disagree at ${at}`);
    }
  });

  it('leaves a function declared at the line as it was', async () => {
    const text = await infer('function_return', 'Plain', 'export async function PLAIN');
    assert.ok(text && /ok: boolean/.test(text), `expected the function body, got: ${text}`);
  });
});
