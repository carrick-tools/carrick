/**
 * carrick#1516: a retyped consumer that fails against the producer's published
 * response but fits what the producer's handler returns is `wider`, not a
 * mismatch.
 *
 * The published type is the compiler's inference, with every literal the
 * handler returns widened (`scope: string`). The unwidened reading keeps
 * them (`scope: 'all' | 'specific'`). The consumer below declares the literal
 * union, and a second consumer narrows on a boolean discriminant; both are
 * correct for what the producer sends and both fail against the widened type.
 *
 * The milder class holds only when the unwidened type raises NOTHING: a
 * narrower type that still does not fit, or none at all, stays a mismatch.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const CLIENT_DECL = `declare module 'http-client' {
  export interface ClientResponse<T = any> {
    data: T;
    status: number;
  }
  export interface ClientInstance {
    get<T = any, R = ClientResponse<T>>(url: string): Promise<R>;
  }
  export function create(): ClientInstance;
}
`;

// Line numbers are read off this text by `lineOf`.
const CONSUMER = `import { create } from 'http-client';

const api = create();

type Scope = 'all' | 'specific';
interface Holiday {
  id: string;
  scope: Scope;
}
declare function setHolidays(next: Holiday[]): void;

export async function loadHolidays(): Promise<void> {
  const response = await api.get('/holidays');
  setHolidays(response.data);
}

export async function sessionId(): Promise<number | undefined> {
  const response = await api.get('/session');
  if (response.data.active) return response.data.session.id;
  return undefined;
}
`;

const WIDENED_HOLIDAYS = '{ id: string; scope: string; }[]';
const UNWIDENED_HOLIDAYS = '{ id: string; scope: "all" | "specific"; }[]';
const WIDENED_SESSION =
  '{ active: boolean; session: null; } | { active: boolean; session: { id: number; }; }';
const UNWIDENED_SESSION =
  '{ active: false; session: null; } | { active: true; session: { id: number; }; }';

interface Outcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'wider' | 'abstain';
  diagnostics: Array<{ line: number; code: number; message: string }>;
  reason?: string;
}

function lineOf(needle: string): number {
  const at = CONSUMER.indexOf(needle);
  assert.ok(at >= 0, `fixture must contain: ${needle}`);
  return CONSUMER.slice(0, at).split('\n').length;
}

describe('carrick#1516: a producer type wider than what its handler returns', () => {
  let client: SidecarClient;
  let repoDir: string;
  let consumerPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1516-retype-'));
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
    consumerPath = path.join(repoDir, 'src', 'client.ts');
    fs.writeFileSync(consumerPath, CONSUMER);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function retype(
    call: string,
    producerType: string,
    unwidened: string | undefined
  ): Promise<Outcome> {
    const line = lineOf(call);
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
      action: 'retype_check',
      request_id: `${call}-${producerType}-${unwidened}`,
      items: [
        {
          item_id: call,
          file_path: consumerPath,
          line_number: line,
          expression_text: call,
          expression_line: line,
          producer_type: producerType,
          ...(unwidened ? { producer_unwidened_type: unwidened } : {}),
          wire: true,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    return res.outcomes![0];
  }

  it('is a mismatch against the published type alone', async () => {
    const out = await retype("api.get('/holidays')", WIDENED_HOLIDAYS, undefined);
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.match(out.diagnostics[0].message, /Type 'string' is not assignable to type 'Scope'/);
  });

  it('is wider when the handler returns only literals the consumer accepts', async () => {
    const out = await retype("api.get('/holidays')", WIDENED_HOLIDAYS, UNWIDENED_HOLIDAYS);
    assert.strictEqual(out.outcome, 'wider', JSON.stringify(out));
    // The diagnostics are the published type's: they say what it declares.
    assert.strictEqual(out.diagnostics.length, 1, JSON.stringify(out.diagnostics));
    assert.strictEqual(out.diagnostics[0].line, lineOf('setHolidays(response.data)'));
    assert.match(out.diagnostics[0].message, /Type 'string' is not assignable to type 'Scope'/);
  });

  it('stays a mismatch when the handler returns a literal the consumer does not accept', async () => {
    const out = await retype(
      "api.get('/holidays')",
      WIDENED_HOLIDAYS,
      '{ id: string; scope: "all" | "none"; }[]'
    );
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
  });

  it('is wider when a boolean discriminant is kept on each return', async () => {
    const widened = await retype("api.get('/session')", WIDENED_SESSION, undefined);
    assert.strictEqual(widened.outcome, 'mismatch', JSON.stringify(widened));
    assert.strictEqual(widened.diagnostics[0].code, 18047, JSON.stringify(widened.diagnostics));

    const out = await retype("api.get('/session')", WIDENED_SESSION, UNWIDENED_SESSION);
    assert.strictEqual(out.outcome, 'wider', JSON.stringify(out));
    assert.strictEqual(out.diagnostics[0].code, 18047);
  });
});
