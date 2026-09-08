/**
 * carrick#770: the capture side of the forward-match guard carrick#766 added
 * to the v1 inferrer.
 *
 * `functionsNearLine` accepts any function starting within two lines of a
 * line-only anchor, in either direction. Looking BACK is free — a function
 * starting before the anchor is one the anchor sits inside or just after.
 * Looking FORWARD is how an anchor reaches a handler that starts a line or two
 * into the registration it names, and it is also how an anchor landing on a
 * statement that declares no function (`export { handler };`) reaches the NEXT
 * declaration in the file.
 *
 * On the v1 side that published a private helper's return type as a route's
 * response contract. Here the exposure is narrower — the caller keeps the first
 * candidate whose PARAMETER NAME resolves, so a neighbour has to declare a
 * parameter of that name to be read at all — but `payload`, `body` and `filter`
 * are exactly the names route helpers share with route handlers. When it
 * happens, the helper's parameter type is published as the route's request
 * contract, silently.
 *
 * The anchor must resolve to the handler or to nothing. Never to the
 * neighbour.
 *
 * Fixtures are synthetic and generically named.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

let repoDir: string;
let outRoot: string;

const ROUTE_TS = `export interface CreateBody {
  name: string;
  quantity: number;
}

/** The route helper's own argument. Not a contract anyone serves. */
export interface InternalFilter {
  field: string;
  direction: 'asc' | 'desc';
}

async function handler(payload: CreateBody) {
  return { id: 'op_1', name: payload.name };
}

// The anchor lands here: a statement that declares no function at all. The
// next declaration starts two lines below and takes a parameter of the same
// name as the handler's.
export { handler };

function buildQuery(payload: InternalFilter) {
  return payload.field + payload.direction;
}

interface Broker {
  register(key: string, run: (payload: CreateBody) => void): void;
}
declare const broker: Broker;

// Control: a registration whose handler signature genuinely starts two lines
// into the statement the anchor names. The guard must not reject this — the
// statement opening on the anchor's line CONTAINS the handler.
broker.register(
  'orders.created',
  (payload: CreateBody) => {
    void payload.quantity;
  }
);
`;

function writeRepo(): void {
  fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
  fs.writeFileSync(
    path.join(repoDir, 'tsconfig.json'),
    JSON.stringify({
      compilerOptions: {
        strict: true,
        rootDir: 'src',
        module: 'esnext',
        moduleResolution: 'bundler',
        target: 'es2022',
        skipLibCheck: true,
      },
      include: ['src'],
    })
  );
  fs.writeFileSync(path.join(repoDir, 'src', 'route.ts'), ROUTE_TS);
}

/** 1-based line of the first source line containing `needle`. */
function lineOf(needle: string): number {
  const lines = ROUTE_TS.split('\n');
  const index = lines.findIndex((l) => l.includes(needle));
  assert.ok(index >= 0, `fixture line not found: ${needle}`);
  return index + 1;
}

describe('capture v2: a line-only anchor never reads the next declaration (carrick#770)', () => {
  let result: CaptureStubResult;
  let surface: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-770-repo-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-770-stub-'));
    writeRepo();
    const anchor = (alias: string, line: number, param: string) => ({
      kind: 'infer' as const,
      alias,
      source_file: 'src/route.ts',
      anchor_origin: 'deterministic-infer' as const,
      line_number: line,
      param_name: param,
    });
    result = captureStub({
      repoRoot: repoDir,
      serviceName: 'route-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: [
        anchor('Binding_Producer_Request', lineOf('export { handler };'), 'payload'),
        anchor('Control_Producer_Request', lineOf("'orders.created'"), 'payload'),
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(
      path.join(result.stub_dir, 'types', 'surface.d.ts'),
      'utf8'
    );
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  const aliasLine = (alias: string): string => {
    const match = surface.match(new RegExp(`export type ${alias} = ([^;]+);`));
    assert.ok(match, `no surface line for ${alias} in:\n${surface}`);
    return match[1].trim();
  };

  it('an anchor on a statement declaring no function never captures the next declaration', () => {
    const text = aliasLine('Binding_Producer_Request');
    assert.doesNotMatch(
      text,
      /InternalFilter|field|direction/,
      `the neighbouring helper's parameter type was published as the route's contract: ${text}`
    );
  });

  it('and abstains rather than guessing, so the row reads unresolved', () => {
    assert.strictEqual(aliasLine('Binding_Producer_Request'), 'unknown');
  });

  it('a handler the anchor line\'s own statement contains still resolves', () => {
    const text = aliasLine('Control_Producer_Request');
    assert.match(
      text,
      /CreateBody|name/,
      `the forward tolerance is kept for candidates the anchor leads to: ${text}`
    );
  });
});
