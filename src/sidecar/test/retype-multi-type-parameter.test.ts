/**
 * A callee with several type parameters is not retyped by writing one type
 * argument: TypeScript does not infer the rest once any is written, so each
 * takes its default instead. A hook whose variables parameter defaults to
 * `void` then rejects the options the source passed it, and that diagnostic,
 * located at the call's own argument, is about the written argument and not
 * about anything the consumer reads.
 *
 * The libraries below are hand-written and synthetic. `useJob` declares
 * `TVariables = void` and `TError = Error` beside `TData`, all three
 * mentioned by the options parameter, so each is inferred from the options
 * object in source. `fetchJob` is the control: one type parameter. `send`
 * is the other control: extra parameters whose defaults are `any`, so a
 * written first argument changes nothing the call can fail on.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const LIB_DECL = `declare module 'job-hooks' {
  export interface JobOptions<TData, TError, TVariables> {
    run: (variables: TVariables) => Promise<TData>;
    onError?: (error: TError) => void;
  }
  export interface JobResult<TData, TError, TVariables> {
    data: TData | undefined;
    error: TError | null;
    start(variables: TVariables): void;
  }
  export function useJob<TData = unknown, TError = Error, TVariables = void>(
    options: JobOptions<TData, TError, TVariables>
  ): JobResult<TData, TError, TVariables>;

  export function fetchJob<TData = unknown>(options: { run: () => Promise<unknown> }): {
    data: TData;
  };

  export interface Sender {
    send<T = any, R = { data: T }, D = any>(url: string, body?: D): Promise<R>;
  }
  export function sender(): Sender;
}
`;

const CONSUMER = `import { useJob, fetchJob, sender } from 'job-hooks';

declare function load(variables: { path: string }): Promise<unknown>;

export function usesHook(): number | undefined {
  const job = useJob({ run: (variables: { path: string }) => load(variables) as Promise<{ x: number }> });
  job.start({ path: 'a' });
  return job.data?.x;
}

export function usesSingle(): number | undefined {
  const job = fetchJob({ run: () => load({ path: 'a' }) });
  return job.data.x;
}

export async function usesSender(): Promise<number> {
  const out = await sender().send('/s', { path: 'a' });
  return out.data.x;
}
`;

const lineOf = (marker: string): number => {
  const lines = CONSUMER.split('\n');
  const at = lines.findIndex((line) => line.includes(marker));
  assert.ok(at >= 0 && lines.findIndex((l, i) => i > at && l.includes(marker)) < 0, marker);
  return at + 1;
};

interface Outcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'abstain';
  diagnostics: Array<{ line: number; code: number; message: string }>;
  reason?: string;
}

describe('a callee with several type parameters', () => {
  let client: SidecarClient;
  let repoDir: string;
  let consumerPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-multi-tp-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
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
    fs.writeFileSync(path.join(repoDir, 'src', 'lib.d.ts'), LIB_DECL);
    consumerPath = path.join(repoDir, 'src', 'jobs.ts');
    fs.writeFileSync(consumerPath, CONSUMER);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function retype(name: string, marker: string, text: string, producer: string): Promise<Outcome> {
    const line = lineOf(marker);
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
      action: 'retype_check',
      request_id: `${name}-${producer}`,
      items: [
        {
          item_id: name,
          file_path: consumerPath,
          line_number: line,
          expression_text: text,
          expression_line: line,
          producer_type: producer,
          wire: true,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    assert.strictEqual(res.outcomes?.length, 1);
    return res.outcomes![0];
  }

  for (const producer of ['{ x: number; }', '{ y: string; }']) {
    it(`abstains on a hook whose other parameters are inferred from its options (${producer})`, async () => {
      const out = await retype('usesHook', 'const job = useJob(', 'useJob(', producer);
      assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
      assert.match(out.reason ?? '', /type parameters/);
    });
  }

  it('still judges a callee with one type parameter', async () => {
    const mismatch = await retype('usesSingle', 'const job = fetchJob(', 'fetchJob(', '{ y: string; }');
    assert.strictEqual(mismatch.outcome, 'mismatch', JSON.stringify(mismatch));
    const agrees = await retype('usesSingle', 'const job = fetchJob(', 'fetchJob(', '{ x: number; }');
    assert.strictEqual(agrees.outcome, 'agrees', JSON.stringify(agrees));
  });

  it('still judges a callee whose extra parameters default to any', async () => {
    const mismatch = await retype('usesSender', "sender().send('/s'", "sender().send('/s', { path: 'a' })", '{ y: string; }');
    assert.strictEqual(mismatch.outcome, 'mismatch', JSON.stringify(mismatch));
    const agrees = await retype('usesSender', "sender().send('/s'", "sender().send('/s', { path: 'a' })", '{ x: number; }');
    assert.strictEqual(agrees.outcome, 'agrees', JSON.stringify(agrees));
  });
});
