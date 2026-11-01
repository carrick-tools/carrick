/**
 * carrick#2041: the literal backfill runs inside the first capture.
 *
 * When a capture demotes an alias (its module's declaration emit was skipped,
 * TS4023) and the scanner holds a usable literal text for it, the stub is
 * captured a second time with that alias re-anchored as a literal at
 * `anchor-backfill` origin. That second capture used to be a second
 * `capture_v2` request in a fresh process: a second program over the service,
 * a second anchor pass, a second emit and a second self-check. One request
 * now carries the texts and answers both stubs, and both must be exactly what
 * the two requests wrote: every file of both stub packages, byte for byte, and
 * every alias record.
 *
 * The reference is the two-request path, run here as the scanner ran it: a
 * capture, then a capture of the anchors the scanner's `backfill_anchors`
 * built from its records.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub } from '../src/capture/index.js';
import { backfillRerun } from '../src/capture/backfill.js';
import type {
  CaptureAliasRecord,
  CaptureAnchorRequest,
  CaptureStubResult,
} from '../src/capture/api.js';

/** An ambient stub whose `export =` hides the instance type: `export default
 * app` in a file importing it cannot be named (TS4023), so that file's
 * declaration emit is skipped and every alias it declares is demoted. */
const AMBIENT_STUB = `declare module "webframe" {
  interface WebframeInstance {
    get(path: string, handler: () => void): this;
  }
  function webframe(): WebframeInstance;
  export = webframe;
}
`;

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'ES2020',
    module: 'commonjs',
    strict: true,
    esModuleInterop: true,
    skipLibCheck: true,
    declaration: true,
    rootDir: './',
  },
  include: ['**/*.ts'],
});

const EVENTS =
  'export interface OrderPlacedEvent {\n' +
  '  id: string;\n' +
  '  total: { amountCents: number; currency: string };\n' +
  '}\n';

function writeRepo(root: string, files: Record<string, string>): string {
  fs.mkdirSync(root, { recursive: true });
  for (const [name, text] of Object.entries(files)) {
    const p = path.join(root, name);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, text);
  }
  return fs.realpathSync(root);
}

const ANCHORS: CaptureAnchorRequest[] = [
  {
    kind: 'symbol',
    alias: 'P_Event',
    symbol_name: 'OrderPlacedEvent',
    source_file: 'src/types/events.ts',
    anchor_origin: 'llm-symbol',
  },
  {
    kind: 'symbol',
    alias: 'P_RouteReply',
    symbol_name: 'RouteReply',
    source_file: 'src/http/routes.ts',
    anchor_origin: 'llm-symbol',
    // A backfill literal does not carry the symbol's mark: the record of the
    // re-anchored alias must read as the two-request path wrote it.
    absent_body_tolerated: true,
  },
  {
    // A bare name a sibling symbol anchor imports resolves through it.
    kind: 'literal',
    alias: 'P_EventByName',
    type_text: 'OrderPlacedEvent',
    anchor_origin: 'deterministic-infer',
  },
];

const TEXTS: Record<string, string> = {
  P_RouteReply: '{ id: string; message: string; }',
  // A healthy alias's text is never used.
  P_Event: '{ never: string }',
};

/** Every file under `dir`, relative path -> bytes. */
function filesOf(dir: string): Map<string, Buffer> {
  const out = new Map<string, Buffer>();
  const walk = (at: string) => {
    for (const entry of fs.readdirSync(at, { withFileTypes: true })) {
      const p = path.join(at, entry.name);
      if (entry.isDirectory()) walk(p);
      else out.set(path.relative(dir, p), fs.readFileSync(p));
    }
  };
  walk(dir);
  return out;
}

function assertSameStub(actualDir: string, expectedDir: string): void {
  const actual = filesOf(actualDir);
  const expected = filesOf(expectedDir);
  assert.deepStrictEqual([...actual.keys()].sort(), [...expected.keys()].sort());
  for (const [rel, bytes] of expected) {
    assert.strictEqual(actual.get(rel)!.toString('utf8'), bytes.toString('utf8'), rel);
  }
}

/** A result with its stub path removed: the only field the run may change. */
function withoutDir(result: CaptureStubResult): Omit<CaptureStubResult, 'stub_dir' | 'backfill'> {
  const { stub_dir: _dir, backfill: _backfill, ...rest } = result;
  return rest;
}

/** The scanner's `backfill_anchors`, as it built the second request. */
function scannerRerun(
  anchors: CaptureAnchorRequest[],
  records: CaptureAliasRecord[],
  texts: Record<string, string>
): CaptureAnchorRequest[] | undefined {
  const backfilled = new Set(
    records
      .filter((r) => r.capture_failure_reason !== undefined && r.anchor_kind !== 'literal')
      .filter((r) => texts[r.alias] !== undefined)
      .map((r) => r.alias)
  );
  if (backfilled.size === 0) return undefined;
  return anchors.map((anchor) =>
    backfilled.has(anchor.alias)
      ? {
          kind: 'literal',
          alias: anchor.alias,
          type_text: texts[anchor.alias],
          anchor_origin: 'anchor-backfill',
          ...('source_file' in anchor && anchor.source_file
            ? { source_file: anchor.source_file }
            : {}),
        }
      : anchor
  );
}

interface Arms {
  repoRoot: string;
  first: CaptureStubResult;
  second: CaptureStubResult;
  fold: CaptureStubResult;
  twoRequestReads: { source: number; stubDeclaration: number };
  foldReads: { source: number; stubDeclaration: number };
}

function runArms(root: string, files: Record<string, string>): Arms {
  const repoRoot = writeRepo(path.join(root, 'repo'), files);
  const healthySource = path.join(repoRoot, 'src/types/events.ts');
  const isStubDeclaration = (file: string) => file.endsWith('/types/src/types/events.d.ts');
  const isSource = (file: string) => file === healthySource;
  const both = (run: () => void) => {
    const counted = { source: 0, stubDeclaration: 0 };
    const originalRead = ts.sys.readFile;
    ts.sys.readFile = (file: string, encoding?: string) => {
      if (isSource(file)) counted.source++;
      if (isStubDeclaration(file)) counted.stubDeclaration++;
      return originalRead(file, encoding);
    };
    try {
      run();
    } finally {
      ts.sys.readFile = originalRead;
    }
    return counted;
  };

  let first!: CaptureStubResult;
  let second!: CaptureStubResult;
  const twoRequestReads = both(() => {
    first = captureStub({
      repoRoot,
      serviceName: 'fold-svc',
      outDir: path.join(root, 'two-request-first'),
      anchors: ANCHORS,
    });
    const rerun = scannerRerun(ANCHORS, first.aliases, TEXTS);
    assert.ok(rerun, 'the fixture demotes an alias the scanner holds a text for');
    second = captureStub({
      repoRoot,
      serviceName: 'fold-svc',
      outDir: path.join(root, 'two-request-second'),
      anchors: rerun,
    });
  });

  let fold!: CaptureStubResult;
  const foldReads = both(() => {
    fold = captureStub({
      repoRoot,
      serviceName: 'fold-svc',
      outDir: path.join(root, 'fold-first'),
      anchors: ANCHORS,
      backfillTexts: TEXTS,
      backfillOutDir: path.join(root, 'fold-second'),
    });
  });
  return { repoRoot, first, second, fold, twoRequestReads, foldReads };
}

function assertSameAsTwoRequests(arms: Arms, root: string): void {
  const { first, second, fold } = arms;
  assert.strictEqual(first.success, true, JSON.stringify(first.errors));
  assert.strictEqual(second.success, true, JSON.stringify(second.errors));
  assert.strictEqual(fold.success, true, JSON.stringify(fold.errors));
  assert.ok(fold.backfill, 'the fold answers the backfilled stub');
  assert.deepStrictEqual(fold.backfill.backfilled, ['P_RouteReply']);
  assert.strictEqual(fold.backfill.result.stub_dir, path.join(root, 'fold-second'));

  assert.deepStrictEqual(withoutDir(fold), withoutDir(first));
  assert.deepStrictEqual(withoutDir(fold.backfill.result), withoutDir(second));
  assertSameStub(fold.stub_dir, first.stub_dir);
  assertSameStub(fold.backfill.result.stub_dir, second.stub_dir);
}

describe('capture v2: the literal backfill runs inside the first capture (#2041)', () => {
  describe('the backfill emits the same tree as the first surface', () => {
    let root: string;
    let arms: Arms;
    before(() => {
      root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-backfill-fold-')));
      arms = runArms(root, {
        'tsconfig.json': TSCONFIG,
        'src/types/stubs.d.ts': AMBIENT_STUB,
        'src/http/routes.ts':
          'import webframe from "webframe";\n' +
          'const app = webframe();\n' +
          'export interface RouteReply { id: string; message: string; }\n' +
          'export default app;\n',
        'src/types/events.ts': EVENTS,
      });
    });
    after(() => fs.rmSync(root, { recursive: true, force: true }));

    it('writes both stubs and both record sets exactly as two requests did', () => {
      assertSameAsTwoRequests(arms, root);
      const rerun = arms.fold.backfill!.result.aliases.find((r) => r.alias === 'P_RouteReply')!;
      assert.strictEqual(rerun.anchor_origin, 'anchor-backfill');
      assert.strictEqual(rerun.self_check, 'ok');
    });

    it('builds the service program once and checks the shared tree once', () => {
      assert.ok(arms.fold.backfill, 'the fold answers the backfilled stub');
      // Two requests: an anchor program and an emit program per capture read
      // the healthy source, and each self-check reads its own stub's copy.
      assert.deepStrictEqual(arms.twoRequestReads, { source: 4, stubDeclaration: 2 });
      // One request: the anchor program and the first emit read the source;
      // the backfill's emit reuses what they parsed, and one self-check
      // program holds both surfaces over the one tree.
      assert.deepStrictEqual(arms.foldReads, { source: 2, stubDeclaration: 1 });
    });
  });

  describe('the backfill drops a module only the demoted alias reached', () => {
    let root: string;
    let arms: Arms;
    before(() => {
      root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-backfill-fold-')));
      arms = runArms(root, {
        'tsconfig.json': TSCONFIG,
        'src/types/stubs.d.ts': AMBIENT_STUB,
        'src/http/shape.ts': 'export interface Shape { kind: "a" | "b"; }\n',
        'src/http/routes.ts':
          'import webframe from "webframe";\n' +
          'import { Shape } from "./shape";\n' +
          'const app = webframe();\n' +
          'export interface RouteReply { id: string; shape: Shape; }\n' +
          'export default app;\n',
        'src/types/events.ts': EVENTS,
      });
    });
    after(() => fs.rmSync(root, { recursive: true, force: true }));

    it('writes both stubs and both record sets exactly as two requests did', () => {
      assertSameAsTwoRequests(arms, root);
      assert.ok(arms.first.emitted_files.includes('types/src/http/shape.d.ts'));
      assert.ok(!arms.second.emitted_files.includes('types/src/http/shape.d.ts'));
    });

    it('builds the service program once', () => {
      assert.ok(arms.fold.backfill, 'the fold answers the backfilled stub');
      assert.strictEqual(arms.twoRequestReads.source, 4);
      assert.strictEqual(arms.foldReads.source, 2);
    });
  });

  describe('no backfill without a demoted alias the texts cover', () => {
    let root: string;
    before(() => {
      root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-backfill-fold-')));
    });
    after(() => fs.rmSync(root, { recursive: true, force: true }));

    it('answers one stub and writes no second', () => {
      const repoRoot = writeRepo(path.join(root, 'repo'), {
        'tsconfig.json': TSCONFIG,
        'src/types/events.ts': EVENTS,
      });
      const result = captureStub({
        repoRoot,
        serviceName: 'fold-svc',
        outDir: path.join(root, 'first'),
        anchors: [ANCHORS[0]],
        backfillTexts: TEXTS,
        backfillOutDir: path.join(root, 'second'),
      });
      assert.strictEqual(result.success, true, JSON.stringify(result.errors));
      assert.strictEqual(result.backfill, undefined);
      assert.ok(!fs.existsSync(path.join(root, 'second')));
    });
  });
});

/** The scanner's `backfill_anchors` rules, now the sidecar's (moved from Rust). */
describe('backfillRerun', () => {
  const symbol = (alias: string): CaptureAnchorRequest => ({
    kind: 'symbol',
    alias,
    symbol_name: 'Notification',
    source_file: 'src/routes.ts',
    anchor_origin: 'llm-symbol',
  });
  const demoted = (request: CaptureAnchorRequest) => ({ request, failureReason: 'emit skipped' });
  const healthy = (request: CaptureAnchorRequest) => ({ request, failureReason: undefined });

  it('re-anchors only a demoted, non-literal alias with a text', () => {
    const anchors: CaptureAnchorRequest[] = [
      symbol('A_demoted'),
      symbol('B_healthy'),
      symbol('C_demoted_no_text'),
      { kind: 'literal', alias: 'D_literal_demoted', type_text: '{ ok: boolean }', anchor_origin: 'deterministic-infer' },
    ];
    const plan = backfillRerun(
      anchors,
      [demoted(anchors[0]), healthy(anchors[1]), demoted(anchors[2]), demoted(anchors[3])],
      {
        A_demoted: '{ id: string; read: boolean; }',
        B_healthy: '{ never: string }',
        D_literal_demoted: '{ ok: boolean }',
      }
    );
    assert.ok(plan);
    assert.deepStrictEqual(plan.backfilled, ['A_demoted']);
    assert.deepStrictEqual(plan.anchors[0], {
      kind: 'literal',
      alias: 'A_demoted',
      type_text: '{ id: string; read: boolean; }',
      anchor_origin: 'anchor-backfill',
      source_file: 'src/routes.ts',
    });
    assert.strictEqual(plan.anchors[1], anchors[1]);
    assert.strictEqual(plan.anchors[2], anchors[2]);
    assert.strictEqual(plan.anchors[3], anchors[3]);
  });

  it('plans nothing without a candidate', () => {
    const a = symbol('A');
    assert.strictEqual(backfillRerun([a], [healthy(a)], { A: '{ ok: boolean }' }), undefined);
    assert.strictEqual(backfillRerun([a], [demoted(a)], {}), undefined);
  });
});
