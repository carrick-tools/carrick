/**
 * carrick#1491: pins why a typed consumer call was unverifiable.
 *
 * With no wrapper rule and an installed client, the inferred type of
 * `api.post<{ x: number }>(url)` is the client's whole envelope, whose
 * defaulted request-data parameter is `any`, and the anchor is the envelope's
 * own symbol: a named generic fares no better. On a checkout where the client
 * does not resolve, the single-call-generic fallback in `inferCallResult`
 * runs, and it anchors a NAMED generic only: an inline literal has no symbol.
 *
 * This is current behaviour, pinned so a change to it is deliberate. If the
 * inferrer starts publishing a call generic as the payload, update this test;
 * the check phase's retype then stops being the only judge of such a call.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const CLIENT_DECL = `declare module 'http-client' {
  export interface ClientResponse<T = any, D = any> {
    data: T;
    status: number;
    sent?: D;
  }
  export interface ClientInstance {
    post<T = any, R = ClientResponse<T>, D = any>(url: string, data?: D): Promise<R>;
  }
  export function create(): ClientInstance;
}
`;

const CONSUMER = (client: string) => `import { create } from '${client}';

const api = create();

export interface Order {
  x: number;
}

export async function inline(): Promise<number> {
  const response = await api.post<{ x: number }>('/p');
  return response.data.x;
}

export async function named(): Promise<number> {
  const response = await api.post<Order>('/p');
  return response.data.x;
}
`;

interface Inferred {
  alias: string;
  type_string: string;
  primary_type_symbol?: string;
}

describe('carrick#1491: an inline call generic is not an anchor', () => {
  let client: SidecarClient;
  let repoDir: string;
  let file: string;
  let bareFile: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1491-generic-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'http-client.d.ts'), CLIENT_DECL);
    file = path.join(repoDir, 'src', 'client.ts');
    bareFile = path.join(repoDir, 'src', 'bare.ts');
    fs.writeFileSync(file, CONSUMER('http-client'));
    fs.writeFileSync(bareFile, CONSUMER('not-installed-client'));
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function infer(
    alias: string,
    line: number,
    text: string,
    at: string = file
  ): Promise<Inferred> {
    const res = await client.send<{ inferred_types?: Inferred[] }>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: at,
          line_number: line,
          infer_kind: 'call_result',
          alias,
          expression_text: text,
          expression_line: line,
        },
      ],
    });
    const found = res.inferred_types?.find((t) => t.alias === alias);
    assert.ok(found, JSON.stringify(res));
    return found!;
  }

  it('with the client installed, publishes the envelope, inline or named', async () => {
    const inline = await infer('inline', 10, "api.post<{ x: number }>('/p')");
    assert.match(inline.type_string, /ClientResponse<\{ x: number; \}, any>/);
    assert.strictEqual(inline.primary_type_symbol, 'ClientResponse');
    const named = await infer('named', 15, "api.post<Order>('/p')");
    assert.match(named.type_string, /ClientResponse<Order, any>/);
    assert.strictEqual(named.primary_type_symbol, 'ClientResponse');
  });

  it('with the client unresolved, anchors a named generic and not an inline one', async () => {
    const inline = await infer('bare-inline', 10, "api.post<{ x: number }>('/p')", bareFile);
    assert.strictEqual(inline.primary_type_symbol ?? undefined, undefined);
    const named = await infer('bare-named', 15, "api.post<Order>('/p')", bareFile);
    assert.strictEqual(named.primary_type_symbol, 'Order');
  });
});
