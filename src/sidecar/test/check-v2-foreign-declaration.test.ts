/**
 * carrick#1047 J1-1: a judged type that reads another service's declaration is
 * unverifiable (`foreign:<side>`).
 *
 * Each service compiles alone and never sees another service's files. The
 * judge loads every probed stub into one program, so a `declare global` in one
 * stub merges into an interface another stub's type is. The pair is then
 * judged on a type neither service compiles, and its verdict depends on which
 * other pairs were probed in the same run.
 *
 * Real captures, judged by `runCheck`. Services:
 *  - x (producer): a `declare global` module file with `interface Shared
 *    { a: string }`, an alias `X_Sent = Shared`, and a control `X2_Sent =
 *    { a: string }`;
 *  - z (third service): a `declare global` file adding `b: number` to
 *    `Shared`, imported by its anchored handler (variant R);
 *  - zc (copy): a `declare global` file with exactly x's `Shared` text;
 *  - web (consumer): `X_Exp = { a: string; b: number }`, `X2_Exp = { a:
 *    string }`, `Z_Exp`; webo (variant O) carries the augmentation itself.
 *
 * Step 0, measured on main b9f26efb before this change (no gate on any; all
 * resolved):
 *  - R {x, x2, z}: x `compatible` (the leak); x2 `compatible`; z `compatible`.
 *  - R {x, x2}: x `incompatible`, codes [2741]; x2 `compatible`.
 *  - O {x}: x `compatible` (the leak).
 *  - Copy {x, zc}: x `incompatible`, codes [2741]; zc `compatible`.
 * (xp, `{ p: Partial<Shared> }`, was added after step 0.)
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import { buildProbe } from '../src/capture/check-probe.js';
import { classifyPair } from '../src/capture/check-classify.js';
import type {
  CaptureAnchorRequest,
  CaptureStubResult,
  CheckPairSpec,
  CheckVerdict,
} from '../src/capture/api.js';

const OPTIONS = {
  target: 'ES2022',
  module: 'ESNext',
  moduleResolution: 'Bundler',
  strict: true,
  skipLibCheck: true,
};

const SHARED_A = ['export {};', 'declare global {', '  interface Shared { a: string }', '}', ''].join('\n');

const SERVICES: Record<string, { files: Record<string, string>; anchors: CaptureAnchorRequest[] }> = {
  x: {
    files: {
      'src/global.ts': SHARED_A,
      'src/handler.ts': [
        "import './global.js';",
        'export type XPayload = Shared;',
        'export type X2Payload = { a: string };',
        'export type XPartial = { p: Partial<Shared> };',
        "export function handler(): Shared { return { a: 'x' } as Shared; }",
        '',
      ].join('\n'),
    },
    anchors: [
      { kind: 'symbol', alias: 'X_Sent', symbol_name: 'XPayload', source_file: 'src/handler.ts', anchor_origin: 'llm-symbol' },
      { kind: 'symbol', alias: 'X2_Sent', symbol_name: 'X2Payload', source_file: 'src/handler.ts', anchor_origin: 'llm-symbol' },
      { kind: 'symbol', alias: 'XP_Sent', symbol_name: 'XPartial', source_file: 'src/handler.ts', anchor_origin: 'llm-symbol' },
    ],
  },
  z: {
    files: {
      'src/aug.ts': [
        'export interface ZMark { b: number }',
        'declare global {',
        '  interface Shared { b: number }',
        '}',
        '',
      ].join('\n'),
      'src/handler.ts': [
        "import type { ZMark } from './aug.js';",
        'export type ZPayload = ZMark;',
        '',
      ].join('\n'),
    },
    anchors: [
      { kind: 'symbol', alias: 'Z_Sent', symbol_name: 'ZPayload', source_file: 'src/handler.ts', anchor_origin: 'llm-symbol' },
    ],
  },
  zc: {
    files: {
      'src/aug.ts': [
        'export interface ZMark { b: number }',
        'declare global {',
        '  interface Shared { a: string }',
        '}',
        '',
      ].join('\n'),
      'src/handler.ts': [
        "import type { ZMark } from './aug.js';",
        'export type ZPayload = ZMark;',
        '',
      ].join('\n'),
    },
    anchors: [
      { kind: 'symbol', alias: 'Z_Sent', symbol_name: 'ZPayload', source_file: 'src/handler.ts', anchor_origin: 'llm-symbol' },
    ],
  },
  web: {
    files: {
      'src/client.ts': [
        'export type XExp = { a: string; b: number };',
        'export type X2Exp = { a: string };',
        'export type ZExp = { b: number };',
        'export type XPExp = { p: { a?: string; b?: number } };',
        '',
      ].join('\n'),
    },
    anchors: [
      { kind: 'symbol', alias: 'X_Exp', symbol_name: 'XExp', source_file: 'src/client.ts', anchor_origin: 'llm-symbol' },
      { kind: 'symbol', alias: 'X2_Exp', symbol_name: 'X2Exp', source_file: 'src/client.ts', anchor_origin: 'llm-symbol' },
      { kind: 'symbol', alias: 'Z_Exp', symbol_name: 'ZExp', source_file: 'src/client.ts', anchor_origin: 'llm-symbol' },
      { kind: 'symbol', alias: 'XP_Exp', symbol_name: 'XPExp', source_file: 'src/client.ts', anchor_origin: 'llm-symbol' },
    ],
  },
  webo: {
    files: {
      'src/global.ts': [
        'export interface WebMark { b: number }',
        'declare global {',
        '  interface Shared { b: number }',
        '}',
        '',
      ].join('\n'),
      'src/client.ts': [
        "import type { WebMark } from './global.js';",
        "export type XExp = { a: string; b: WebMark['b'] };",
        '',
      ].join('\n'),
    },
    anchors: [
      { kind: 'symbol', alias: 'X_Exp', symbol_name: 'XExp', source_file: 'src/client.ts', anchor_origin: 'llm-symbol' },
    ],
  },
};

function capture(root: string, service: string): CaptureStubResult {
  const repoRoot = path.join(root, `${service}-repo`);
  const { files, anchors } = SERVICES[service];
  for (const [rel, text] of Object.entries({
    ...files,
    'tsconfig.json': JSON.stringify({ compilerOptions: OPTIONS, include: ['src'] }),
  })) {
    fs.mkdirSync(path.dirname(path.join(repoRoot, rel)), { recursive: true });
    fs.writeFileSync(path.join(repoRoot, rel), text);
  }
  const result = captureStub({
    repoRoot,
    serviceName: service,
    outDir: path.join(root, `${service}-stub`),
    anchors,
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return result;
}

const pair = (key: string, producer: string, pAlias: string, consumer: string, cAlias: string): CheckPairSpec => ({
  pair_key: key,
  protocol: 'http',
  type_kind: 'response',
  producer: { service_name: producer, alias: pAlias },
  consumer: { service_name: consumer, alias: cAlias },
});

/** The verdict fields compared between runs (diagnostic text excluded, #735). */
const fields = (v: CheckVerdict | undefined) =>
  v && {
    bucket: v.bucket,
    gate: v.gate,
    resolved: v.resolved,
    unresolved_reason: v.unresolved_reason,
    unresolved_side: v.unresolved_side,
    codes: v.codes,
  };

describe('check phase: a type that reads another service declaration (carrick#1047 J1-1)', () => {
  let root: string;
  const stubs = new Map<string, CaptureStubResult>();
  const runs = new Map<string, Map<string, CheckVerdict>>();

  const judge = async (name: string, services: string[], pairs: CheckPairSpec[]): Promise<void> => {
    const result = await runCheck({
      stubs: services.map((s) => ({ service_name: s, stub_dir: stubs.get(s)!.stub_dir })),
      pairs,
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    runs.set(name, new Map(result.verdicts.map((v) => [v.pair_key, v])));
  };
  const verdict = (run: string, key: string): CheckVerdict => {
    const v = runs.get(run)?.get(key);
    assert.ok(v, `no verdict for ${key} in run ${run}`);
    return v;
  };

  const X = pair('x', 'x', 'X_Sent', 'web', 'X_Exp');
  const X2 = pair('x2', 'x', 'X2_Sent', 'web', 'X2_Exp');
  const XP = pair('xp', 'x', 'XP_Sent', 'web', 'XP_Exp');
  const Z = pair('z', 'z', 'Z_Sent', 'web', 'Z_Exp');
  const ZC = pair('zc', 'zc', 'Z_Sent', 'web', 'Z_Exp');
  const XO = pair('x', 'x', 'X_Sent', 'webo', 'X_Exp');

  before(async () => {
    root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1047-foreign-')));
    for (const service of Object.keys(SERVICES)) stubs.set(service, capture(root, service));
    await Promise.all([
      judge('R-both', ['x', 'z', 'web'], [X, X2, XP, Z]),
      judge('R-alone', ['x', 'z', 'web'], [X, X2]),
      judge('O', ['x', 'webo'], [XO]),
      judge('copy', ['x', 'zc', 'web'], [X, ZC]),
    ]);
    if (process.env.CARRICK_FOREIGN_DUMP) {
      for (const [run, verdicts] of runs) {
        for (const [key, v] of verdicts) console.log(run, key, JSON.stringify(fields(v)));
      }
    }
  });

  after(() => {
    if (root) fs.rmSync(root, { recursive: true, force: true });
  });

  it('R, both pairs probed: x reads z declaration, so it is foreign:producer naming z', () => {
    const v = verdict('R-both', 'x');
    assert.strictEqual(v.bucket, 'unverifiable');
    assert.strictEqual(v.gate, 'foreign:producer');
    assert.strictEqual(v.unresolved_side, 'producer');
    assert.strictEqual(v.resolved, false);
    assert.match(v.unresolved_reason ?? '', /\bz\b/);
  });

  it('R, both probed: a mapped member of the merged interface (Partial) is found below the root', () => {
    const v = verdict('R-both', 'xp');
    assert.strictEqual(v.gate, 'foreign:producer');
    assert.match(v.unresolved_reason ?? '', /\bz\b/);
  });

  it('R, x pair alone: z declaration is not loaded, so the verdict is main (incompatible)', () => {
    const v = verdict('R-alone', 'x');
    assert.strictEqual(v.bucket, 'incompatible');
    assert.strictEqual(v.gate, undefined);
  });

  it('O: the other side of the pair declares it, so foreign:producer naming webo', () => {
    const v = verdict('O', 'x');
    assert.strictEqual(v.bucket, 'unverifiable');
    assert.strictEqual(v.gate, 'foreign:producer');
    assert.strictEqual(v.unresolved_side, 'producer');
    assert.match(v.unresolved_reason ?? '', /\bwebo\b/);
  });

  it('control x2: reads no augmented symbol, same verdict in both runs and on main', () => {
    assert.deepStrictEqual(fields(verdict('R-both', 'x2')), fields(verdict('R-alone', 'x2')));
    assert.strictEqual(verdict('R-both', 'x2').bucket, 'compatible');
    assert.strictEqual(verdict('R-both', 'x2').resolved, true);
  });

  it('z own pair reads its own declaration only, so it is judged as on main', () => {
    assert.strictEqual(verdict('R-both', 'z').bucket, 'compatible');
    assert.strictEqual(verdict('R-both', 'z').gate, undefined);
  });

  it('copy: a sibling declaration with the same text as x own is not foreign', () => {
    const v = verdict('copy', 'x');
    assert.strictEqual(v.bucket, 'incompatible');
    assert.strictEqual(v.gate, undefined);
  });
});

describe('step 1b of the classifier: foreign:<side> (carrick#1047 J1-1)', () => {
  const plan = buildProbe(pair('k', 'x', 'X_Sent', 'web', 'X_Exp'), (s) => `@carrick/${s}`);
  const classify = (foreign: Parameters<typeof classifyPair>[0]['foreign']) =>
    classifyPair({
      plan,
      probeDiags: [],
      poisonReason: () => undefined,
      scrubCtx: { workspaceRoot: '/tmp/ws', packageLabelOf: () => undefined },
      deepFindings: { sent: [], expected: [] },
      foreign,
    });

  it('a walk out of budget fails closed with the too-deep reason', () => {
    const v = classify({ consumer: { exhaustedAt: 'items<0>' } });
    assert.strictEqual(v.bucket, 'unverifiable');
    assert.strictEqual(v.gate, 'foreign:consumer');
    assert.strictEqual(v.unresolved_side, 'consumer');
    assert.strictEqual(v.unresolved_reason, "the consumer type is too deep or wide to verify at 'items<0>'");
  });

  it('no program to walk in fails closed', () => {
    const v = classify({ producer: 'unwalked' });
    assert.strictEqual(v.gate, 'foreign:producer');
    assert.strictEqual(v.resolved, false);
    assert.strictEqual(v.unresolved_reason, 'the producer type may use declarations from another service');
  });

  it('both sides hit: the producer is named first', () => {
    const v = classify({
      consumer: { service: 'z', path: '' },
      producer: { service: 'web', path: 'a' },
    });
    assert.strictEqual(v.gate, 'foreign:producer');
    assert.match(v.unresolved_reason ?? '', /\bweb\b/);
  });

  it('poison outranks it', () => {
    const v = classifyPair({
      plan,
      probeDiags: [],
      poisonReason: (service) => (service === 'web' ? 'the consumer stub does not typecheck' : undefined),
      scrubCtx: { workspaceRoot: '/tmp/ws', packageLabelOf: () => undefined },
      foreign: { producer: { service: 'z', path: '' } },
    });
    assert.strictEqual(v.gate, 'poison:consumer');
  });

  it('no finding leaves the verdict alone', () => {
    assert.strictEqual(classify(undefined).bucket, 'compatible');
    assert.strictEqual(classify({}).bucket, 'compatible');
  });
});
