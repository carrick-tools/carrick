/**
 * The retype's procedure form on a call whose callee is a bare identifier: a
 * function the library hands out as a value (`save(...)`, a hook's alias of
 * it, or the procedure itself called directly), not a member of a client
 * (`client.notes.save.mutate(...)`). The item names the procedure by a single
 * segment; the callee's own name is the consumer's (an alias), so the call is
 * told apart by its shape (an identifier callee) and its line alone.
 *
 * Everything after the call is located is the existing judge: the consumer's
 * own reads against the call's own type, with the result erased.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const KIT = `
export interface Ctx<D> { data: D }
export interface Stub<I, R> { (opts: { data: I }): Promise<Awaited<R>> }
export interface Built<I> {
  handler<R>(fn: (c: Ctx<I>) => R): Stub<I, R>;
  validate<J>(v: (d: J) => unknown): Built<J>;
}
export declare const defineFn: (opts?: { method?: "GET" | "POST" }) => Built<undefined>;
export declare function useFn<T extends (...a: any[]) => Promise<any>>(fn: T): (...a: Parameters<T>) => ReturnType<T>;
`;

const NOTES = `import { defineFn } from "fixture-fn";
export const listNotes = defineFn({ method: "GET" }).handler(async () => [{ id: 1 }]);
export const saveNote = defineFn({ method: "POST" })
  .validate((d: { body: string }) => d)
  .handler(async ({ data }) => ({ ok: true }));
`;

const CLIENT_KIT = `export declare const client: {
  notes: { save: { mutate(input?: unknown): Promise<{ ok: boolean }> } };
  save: { mutate(input?: unknown): Promise<{ ok: boolean }> };
};
`;

/** The call under test is always on line 4. */
const CALL_LINE = 4;
const VIEWS: Record<string, string> = {
  'alias-missing.ts': `import { useFn } from "fixture-fn";
import { saveNote } from "../notes.js";
const save = useFn(saveNote);
const out = await save({ data: { body: "x" } });
const bad = out.missing;
export { bad };
`,
  'alias-ok.ts': `import { useFn } from "fixture-fn";
import { saveNote } from "../notes.js";
const save = useFn(saveNote);
const out = await save({ data: { body: "x" } });
const good = out.ok;
export { good };
`,
  'direct-missing.ts': `import { listNotes } from "../notes.js";
const x = 1;
export { x };
const rows = await listNotes();
const bad = rows[0].missing;
export { bad };
`,
  'direct-ok.ts': `import { listNotes } from "../notes.js";
const x = 1;
export { x };
const rows = await listNotes();
const good = rows[0].id;
export { good };
`,
  'twice.ts': `import { useFn } from "fixture-fn";
import { saveNote } from "../notes.js";
const save = useFn(saveNote);
const out = await save({ data: { body: String(await save({ data: { body: "y" } })) } });
const bad = out.missing;
export { bad };
`,
  'member-missing.ts': `import { client } from "../client-kit.js";
const x = 1;
export { x };
const out = await client.notes.save.mutate({ body: "x" });
const bad = out.missing;
export { bad };
`,
  'member-single.ts': `import { client } from "../client-kit.js";
const x = 1;
export { x };
const out = await client.save.mutate({ body: "x" });
const bad = out.missing;
export { bad };
`,
};

interface Outcome {
  outcome: string;
  reason?: string;
  diagnostics: Array<{ line: number; code: number; message: string }>;
}

describe('retype procedure form: a bare-identifier callee', () => {
  let repoDir: string;
  let client: SidecarClient;

  const write = (rel: string, text: string) => {
    const file = path.join(repoDir, rel);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
  };

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'retype-bare-callee-'));
    write(
      'tsconfig.json',
      JSON.stringify({
        compilerOptions: {
          target: 'ES2022',
          module: 'NodeNext',
          moduleResolution: 'NodeNext',
          strict: true,
          noEmit: true,
          skipLibCheck: true,
        },
        include: ['src/**/*'],
      })
    );
    write('package.json', '{ "name": "bare-callee", "type": "module" }\n');
    write('node_modules/fixture-fn/package.json', '{ "name": "fixture-fn", "version": "1.0.0", "types": "index.d.ts" }\n');
    write('node_modules/fixture-fn/index.d.ts', KIT);
    write('src/notes.ts', NOTES);
    write('src/client-kit.ts', CLIENT_KIT);
    for (const [name, text] of Object.entries(VIEWS)) write(`src/web/${name}`, text);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function judge(
    view: string,
    producerType: string,
    procedurePath: string,
    line = CALL_LINE
  ): Promise<Outcome> {
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
      action: 'retype_check',
      request_id: `${view}-${procedurePath}`,
      items: [
        {
          item_id: view,
          file_path: path.join(repoDir, 'src', 'web', view),
          line_number: line,
          producer_type: producerType,
          wire: true,
          procedure_path: procedurePath,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    return res.outcomes![0];
  }

  it('finds the read of a field the alias call does not return', async () => {
    const out = await judge('alias-missing.ts', '{ ok: boolean }', 'saveNote');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.deepStrictEqual(
      out.diagnostics.map((d) => ({ line: d.line, code: d.code })),
      [{ line: 5, code: 2339 }]
    );
  });

  it('agrees when the alias call result is read as returned', async () => {
    const out = await judge('alias-ok.ts', '{ ok: boolean }', 'saveNote');
    assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
  });

  it('finds the read of a missing field on a direct call of the procedure', async () => {
    const out = await judge('direct-missing.ts', '{ id: number }[]', 'listNotes');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.deepStrictEqual(
      out.diagnostics.map((d) => ({ line: d.line, code: d.code })),
      [{ line: 5, code: 2339 }]
    );
  });

  it('agrees on a direct call read as returned', async () => {
    const out = await judge('direct-ok.ts', '{ id: number }[]', 'listNotes');
    assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
  });

  it('abstains when two bare calls start on the line', async () => {
    const out = await judge('twice.ts', '{ ok: boolean }', 'saveNote');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /two calls to the procedure/);
  });

  it('does not read a bare callee as a dotted procedure', async () => {
    const out = await judge('alias-missing.ts', '{ ok: boolean }', 'notes.saveNote');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /could not be located/);
  });

  it('locates a client member call exactly as before (dotted path)', async () => {
    const out = await judge('member-missing.ts', '{ ok: boolean }', 'notes.save');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
  });

  it('locates a client member call exactly as before (single segment)', async () => {
    const out = await judge('member-single.ts', '{ ok: boolean }', 'save');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
  });
});
