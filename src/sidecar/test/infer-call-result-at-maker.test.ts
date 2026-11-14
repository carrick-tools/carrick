/**
 * carrick#4375: a call's result can be generic in a maker. `makeLookup<T>`
 * returns a function whose request call answers `Promise<T>`; read inside
 * the maker, the result holds the open `T`. The scanner names the maker call
 * beside the result (`through_call`), and the result is then the type that
 * call instantiates, read by the same path a built body takes
 * (carrick#4128): `{ count: number }` at `makeLookup<{ count: number }>(...)`.
 * A call that leaves `T` open, and a result read with no caller, stay a
 * decided `unknown`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const MAKER_TS = `declare function request<R>(url: string): Promise<R>;

export function makeLookup<T>(root: string) {
  return () => request<T>(\`\${root}/read\`);
}

export function makeFixed(root: string) {
  return () => request<{ plain: boolean }>(\`\${root}/read\`);
}
`;

const USE_TS = `import { makeLookup, makeFixed } from './maker';
export const stock = makeLookup<{ count: number }>('/stock');
export const relay = <U>() => makeLookup<U>('/stock');
export const fixed = makeFixed('/fixed');
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ reason: string }>;
  }>;
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#4375: a call result generic in a maker takes the maker call', () => {
  let client: SidecarClient;
  let repoDir: string;
  let makerPath: string;
  let usePath: string;

  const spanOf = (source: string, text: string): { start: number; end: number } => {
    const start = source.indexOf(text);
    assert.ok(start >= 0, `fixture holds no ${JSON.stringify(text)}`);
    return { start, end: start + text.length };
  };
  const lineAt = (source: string, offset: number): number => source.slice(0, offset).split('\n').length;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4375-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', skipLibCheck: true },
        include: ['src'],
      })
    );
    makerPath = path.join(repoDir, 'src', 'maker.ts');
    usePath = path.join(repoDir, 'src', 'use.ts');
    fs.writeFileSync(makerPath, MAKER_TS);
    fs.writeFileSync(usePath, USE_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-4375', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (alias: string, callText: string, call: string | undefined) => {
    const at = spanOf(MAKER_TS, callText);
    const through = call === undefined ? undefined : spanOf(USE_TS, call);
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: makerPath,
          line_number: lineAt(MAKER_TS, at.start),
          span_start: at.start,
          span_end: at.end,
          infer_kind: 'call_result',
          alias,
          ...(through
            ? { through_call: { file_path: usePath, span_start: through.start, span_end: through.end } }
            : {}),
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(inferred, `no answer for ${alias}: ${JSON.stringify(response.errors)}`);
    return { text: collapse(inferred.type_string), provenance: inferred.any_provenance ?? [] };
  };

  it('reads the result as the maker call instantiates it', async () => {
    const { text } = await infer('Stock', 'request<T>(`${root}/read`)', "makeLookup<{ count: number }>('/stock')");
    assert.match(text, /^\{ count: number;? \}$/, text);
  });

  it('answers a decided unknown where the maker call passes its own type parameter', async () => {
    const { text, provenance } = await infer('Relay', 'request<T>(`${root}/read`)', "makeLookup<U>('/stock')");
    assert.strictEqual(text, 'unknown');
    assert.deepStrictEqual(provenance.map((p) => p.reason), ['bound_by_callers']);
  });

  it('answers a decided unknown for the maker own row with no caller', async () => {
    const { text, provenance } = await infer('Own', 'request<T>(`${root}/read`)', undefined);
    assert.strictEqual(text, 'unknown');
    assert.deepStrictEqual(provenance.map((p) => p.reason), ['bound_by_callers']);
  });

  it('leaves a result with no type parameter as it reads, caller or not', async () => {
    const withCaller = await infer('FixedA', 'request<{ plain: boolean }>(`${root}/read`)', "makeFixed('/fixed')");
    const without = await infer('FixedB', 'request<{ plain: boolean }>(`${root}/read`)', undefined);
    assert.match(withCaller.text, /^\{ plain: boolean;? \}$/, withCaller.text);
    assert.strictEqual(withCaller.text, without.text);
  });
});
