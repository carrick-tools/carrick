/**
 * PROTOTYPE, never merged (one-engine step 0, S0-c): the walk-first probe.
 *
 * With `CARRICK_PROBE_WALK_FIRST` set, an analysis process walks its default
 * program's repo files with the position rows' walker when that program is
 * first built. `kept` keeps the walk's checker; `fresh` keeps the parsed
 * source files and gives both programs a new checker. A process scoped to
 * the workspace source (the walk's own) never runs the probe.
 */

import { describe, it, before, after, afterEach } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { ProjectLoader } from '../src/project-loader.js';
import { SidecarClient } from './helpers.js';

/** Unions of string literals, an enum, an overload and a mapped type over a contract. */
const SERVICE_TS = `export enum Status { Open = 'open', Closed = 'closed' }
export interface Order { id: string; total: number; status: Status; tag: 'a' | 'b' | 'c' }
export type OrderRef = Pick<Order, 'id'>;
export function pick(kind: 'x'): number;
export function pick(kind: 'y' | 'z'): string;
export function pick(kind: 'x' | 'y' | 'z'): number | string { return kind === 'x' ? 1 : kind; }
export function handle(body: Order | OrderRef | null, mode: 'fast' | 'slow' | Status) { // MARK_HANDLE
  const ref: OrderRef = { id: body?.id ?? '' };
  return { ref, mode, picked: pick('y'), status: Status.Open as Status | 'none' }; // MARK_RETURN
}
`;

const OTHER_TS = `import { handle, Status } from './service';
export const answer = handle(null, Status.Closed); // MARK_CALL
`;

function lineOf(text: string, marker: string): number {
  return text.split('\n').findIndex((line) => line.includes(marker)) + 1;
}

function makeRepo(): string {
  const repo = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-walk-first-')));
  fs.mkdirSync(path.join(repo, 'src'));
  fs.writeFileSync(
    path.join(repo, 'tsconfig.json'),
    JSON.stringify({
      compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', skipLibCheck: true },
      include: ['src'],
    })
  );
  fs.writeFileSync(path.join(repo, 'src', 'service.ts'), SERVICE_TS);
  fs.writeFileSync(path.join(repo, 'src', 'other.ts'), OTHER_TS);
  return repo;
}

describe('walk-first probe: the loader', () => {
  let repo: string;
  before(() => {
    repo = makeRepo();
  });
  after(() => fs.rmSync(repo, { recursive: true, force: true }));
  afterEach(() => {
    delete process.env.CARRICK_PROBE_WALK_FIRST;
  });

  function built(options: { workspaceSource?: boolean } = {}): ProjectLoader {
    const loader = new ProjectLoader({ repoRoot: repo, ...options });
    assert.ok(loader.load().success);
    loader.getProject();
    return loader;
  }

  it('does nothing with the variable unset', () => {
    assert.strictEqual(built().walkFirstRecord, undefined);
  });

  it('never runs in the position rows process (workspace source)', () => {
    process.env.CARRICK_PROBE_WALK_FIRST = 'kept';
    assert.strictEqual(built({ workspaceSource: true }).walkFirstRecord, undefined);
  });

  it('kept: walks the repo files and keeps the program and its checker', () => {
    process.env.CARRICK_PROBE_WALK_FIRST = 'kept';
    const loader = built();
    const record = loader.walkFirstRecord;
    assert.ok(record);
    assert.strictEqual(record.files, 2);
    assert.ok(record.rows > 0);
    assert.ok(loader.isBuilt(''), 'the walk must not release the default program');
    const project = loader.getProject();
    const now = project.getLanguageService().compilerObject.getProgram();
    assert.strictEqual(now, record.walked, 'kept keeps the walked program');
    assert.strictEqual(now!.getTypeChecker(), record.walked.getTypeChecker());
  });

  it('fresh: a new checker on both programs over the same parsed source files', () => {
    process.env.CARRICK_PROBE_WALK_FIRST = 'fresh';
    const loader = built();
    const record = loader.walkFirstRecord;
    assert.ok(record);
    const walkedChecker = record.walked.getTypeChecker();
    const project = loader.getProject();
    assert.ok(loader.isBuilt(''));
    const now = project.getLanguageService().compilerObject.getProgram()!;
    assert.notStrictEqual(now, record.walked, 'fresh gives the language service a new program');
    assert.notStrictEqual(now.getTypeChecker(), walkedChecker, 'and a new checker');
    assert.deepStrictEqual([...now.getRootFileNames()], [...record.walked.getRootFileNames()]);
    const walkedFiles = record.walked.getSourceFiles();
    assert.strictEqual(now.getSourceFiles().length, walkedFiles.length);
    for (const file of walkedFiles) {
      assert.strictEqual(now.getSourceFile(file.fileName), file, `${file.fileName} was parsed again`);
    }
    const tsMorph = project.getProgram().compilerObject;
    assert.notStrictEqual(tsMorph.getTypeChecker(), walkedChecker);
    for (const file of walkedFiles) {
      assert.strictEqual(tsMorph.getSourceFile(file.fileName), file, `${file.fileName} was parsed again (ts-morph program)`);
    }
  });
});

describe('walk-first probe: infer answers over a union fixture', () => {
  let repo: string;
  before(() => {
    repo = makeRepo();
  });
  after(() => fs.rmSync(repo, { recursive: true, force: true }));

  async function answers(variant: string | undefined): Promise<string> {
    const client = new SidecarClient();
    await client.start(variant === undefined ? {} : { CARRICK_PROBE_WALK_FIRST: variant });
    try {
      await client.send({ action: 'init', request_id: 'init', repo_root: repo });
      const service = path.join(repo, 'src', 'service.ts');
      const other = path.join(repo, 'src', 'other.ts');
      const res = await client.send<{ inferred_types?: unknown[] }>({
        action: 'infer',
        request_id: 'infer',
        requests: [
          { file_path: service, line_number: lineOf(SERVICE_TS, 'MARK_HANDLE'), infer_kind: 'function_param', param_name: 'body', alias: 'Body' },
          { file_path: service, line_number: lineOf(SERVICE_TS, 'MARK_HANDLE'), infer_kind: 'function_param', param_name: 'mode', alias: 'Mode' },
          { file_path: service, line_number: lineOf(SERVICE_TS, 'MARK_RETURN'), infer_kind: 'response_body', alias: 'Ret' },
          { file_path: other, line_number: lineOf(OTHER_TS, 'MARK_CALL'), infer_kind: 'call_result', expression_text: 'handle(null, Status.Closed)', alias: 'Call' },
        ],
      });
      return JSON.stringify(res.inferred_types ?? []).replace(/"(?:infer|elapsed|duration)[a-z_]*_ms":\d+,?/g, '');
    } finally {
      await client.stop();
    }
  }

  it('kept and fresh answer byte for byte as an unset process', async () => {
    const unset = await answers(undefined);
    assert.ok(unset.length > 2, `no answers: ${unset}`);
    assert.strictEqual(await answers('kept'), unset, 'kept');
    assert.strictEqual(await answers('fresh'), unset, 'fresh');
  });
});
