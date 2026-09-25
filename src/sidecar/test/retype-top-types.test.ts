/**
 * carrick#1514: the retype check never reports a mismatch that an unresolved
 * type caused, and when a member of the producer's response is unresolved it
 * says which one instead.
 *
 * The retype states the producer's response in the CONSUMER's own program,
 * under the consumer's compiler options. A consumer without
 * `strictNullChecks` is common (a mobile app's base config sets no `strict`),
 * and there `null` is assignable to every type, so the JSON wire transform's
 * `null extends { toJSON(): infer R }` test held and `R` came back `unknown`.
 * A producer member typed `null` in one branch of a union then read as
 * `unknown`, and a field the producer does return was flagged as missing.
 *
 * Every other retype fixture, and the check workspace, is `strict: true`,
 * which is why none of them saw it. This one sets `strictNullChecks: false`
 * explicitly: the compiler's default for it changes between major versions.
 *
 * The client below is hand-written and synthetic.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { ts as morphTs } from 'ts-morph';
import ts from 'typescript';
import { SidecarClient } from './helpers.js';

const CLIENT_DECL = `declare module 'http-client' {
  export interface ClientResponse<T = any> {
    data: T;
    status: number;
  }
  export interface ClientInstance {
    get<T = any, R = ClientResponse<T>>(url: string): Promise<R>;
  }
  export function create(options?: { baseURL?: string }): ClientInstance;
}
`;

const CONSUMER = `import { create } from 'http-client';

const api = create({ baseURL: 'http://localhost:3000' });

declare function remember(id: string | null): void;

export async function readsPending(): Promise<void> {
  const response = await api.get('/session');
  if (response.data.active) {
    remember(response.data.session.pendingId);
  }
}

export async function readsAbsent(): Promise<void> {
  const response = await api.get('/session');
  if (response.data.active) {
    remember(response.data.session.absentId);
  }
}
`;

/** 1-based line of the `nth` line containing `marker` (0-based `nth`). */
function lineOf(marker: string, nth = 0): number {
  const lines = CONSUMER.split('\n');
  let seen = 0;
  for (let i = 0; i < lines.length; i++) {
    if (!lines[i].includes(marker)) continue;
    if (seen++ === nth) return i + 1;
  }
  throw new Error(`no line ${nth} contains ${marker}`);
}

const CALL = "api.get('/session')";
const CASES = {
  pending: { line: lineOf(CALL, 0), read: lineOf('.pendingId') },
  absent: { line: lineOf(CALL, 1), read: lineOf('.absentId') },
} as const;

/** The producer's response: the session member is `null` in one branch. */
const SESSION =
  '{ active: boolean; session: null; } | { active: boolean; session: { id: string; pendingId: null | string; }; }';

interface Outcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'abstain';
  diagnostics: Array<{ line: number; code: number; message: string }>;
  reason?: string;
}

describe('carrick#1514: the retype in a program without strictNullChecks', () => {
  let client: SidecarClient;
  let repoDir: string;
  let consumerPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1514-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: false,
          strictNullChecks: false,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'http-client.d.ts'), CLIENT_DECL);
    consumerPath = path.join(repoDir, 'src', 'screen.ts');
    fs.writeFileSync(consumerPath, CONSUMER);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function retype(name: keyof typeof CASES, producerType: string): Promise<Outcome> {
    const c = CASES[name];
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
      action: 'retype_check',
      request_id: `${name}-${producerType}`,
      items: [
        {
          item_id: name,
          file_path: consumerPath,
          line_number: c.line,
          expression_text: CALL,
          expression_line: c.line,
          producer_type: producerType,
          wire: true,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    assert.strictEqual(res.outcomes?.length, 1);
    return res.outcomes![0];
  }

  it('agrees when the field the consumer reads is on the producer, beside a null branch', async () => {
    const out = await retype('pending', SESSION);
    assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
  });

  it('agrees with an undefined branch as it does with a null one', async () => {
    const out = await retype('pending', SESSION.replace('session: null', 'session: undefined'));
    assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
  });

  it('still flags a field the producer does not return, against the real type', async () => {
    const out = await retype('absent', SESSION);
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.strictEqual(out.diagnostics.length, 1, JSON.stringify(out.diagnostics));
    const [d] = out.diagnostics;
    assert.strictEqual(d.line, CASES.absent.read);
    assert.strictEqual(d.code, 2339);
    assert.match(d.message, /Property 'absentId' does not exist on type '\{ id: string; pendingId: string; \}'/);
  });

  it('does not compare a member that reads as unknown, and names it', async () => {
    const out = await retype('pending', '{ active: boolean; session: unknown; }');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.deepStrictEqual(out.diagnostics, []);
    assert.match(out.reason ?? '', /'unknown' at 'session'/);
  });

  it('does not compare a member that reads as any, and names it', async () => {
    // `any` never errors, so without this the read would "agree" with a
    // member nobody could see.
    const out = await retype(
      'pending',
      '{ active: boolean; session: { id: string; pendingId: string; extra: any; }; }'
    );
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /'any' at 'session\.extra'/);
  });

  it('does not compare a response that reads as unknown or any as a whole', async () => {
    const unknown = await retype('pending', 'unknown');
    assert.strictEqual(unknown.outcome, 'abstain', JSON.stringify(unknown));
    assert.match(unknown.reason ?? '', /reads as 'unknown' in the consumer's program/);
    const any = await retype('pending', 'any');
    assert.strictEqual(any.outcome, 'abstain', JSON.stringify(any));
    assert.match(any.reason ?? '', /reads as 'any' in the consumer's program/);
  });

  it('still judges a type deeper than the walk can finish', async () => {
    // The walk gives up past its depth budget having found nothing to name,
    // so the compiler's own answer stands.
    let deep = '{ leaf: string; }';
    for (let i = 0; i < 20; i++) deep = `{ next: ${deep}; }`;
    const out = await retype(
      'pending',
      `{ active: boolean; session: { id: string; pendingId: string; chain: ${deep}; }; }`
    );
    assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
  });
});

describe('carrick#1514: the any/unknown walk the retype borrows', () => {
  it('reads the same flags in both compiler copies', () => {
    // The walk is typed against the capture bundle's `typescript` and is
    // handed the retype project's (ts-morph's own copy). It reads only these
    // two enums; if a version bump moves either, the walk misreads types.
    assert.deepStrictEqual({ ...morphTs.TypeFlags }, { ...ts.TypeFlags });
    assert.deepStrictEqual({ ...morphTs.ObjectFlags }, { ...ts.ObjectFlags });
  });
});
