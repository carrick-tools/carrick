/**
 * carrick#4709, the capture half: a request body whose model text is not in
 * the file must not be read off whatever the line names.
 *
 * Where the compiler engine's inferrer reads nothing for a request body, the
 * capture re-runs the anchor. Its text locator finds nothing when the model's
 * text is not in the file, and it fell back to the first expression on the
 * hint line: the route's registration call (the server instance), or an
 * `as const` tuple the handler builds on that line. Neither is the body. A
 * request body asked by line alone takes no type at the scanner
 * (`derive_capture_anchors`: `not_located`), and a text that missed leaves
 * the capture with exactly that ask, so it answers the same.
 *
 * A text that is found still binds, and another kind's line fallback keeps
 * its reading.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

const SCHEMA_TS = `export interface Schema<T> {
  parse(value: unknown): T;
}
export function shape<T>(): Schema<T> {
  return { parse: (value: unknown) => value as T };
}
export interface Incoming { body: unknown }
export interface Outgoing { code(status: number): this }
export type Handler = (request: Incoming, reply: Outgoing) => unknown;
export interface Server {
  post(path: string, handler: Handler): this;
}
`;

const ROUTES_TS = `import { shape } from './schema.js';
import type { Server, Outgoing } from './schema.js';

const OrderBody = shape<{ title: string; items: string[] }>();

async function guarded<T>(reply: Outgoing, run: () => Promise<T>): Promise<T> {
  reply.code(200);
  return run();
}

export function orderRoutes(app: Server): void {
  app.post('/wrapped-parsed', async (request, reply) =>
    guarded(reply, async () => {
      const body = OrderBody.parse(request.body);
      const pairs = body.items.map((item) => [item, 'item'] as const);
      return { count: pairs.length };
    })
  );

  app.post('/direct-cast', async (request) => {
    const typed = request.body as { title: string };
    return { saved: typed.title };
  });
}

export function total(values: number[]): number {
  return values.length;
}
`;

const LINES = ROUTES_TS.split('\n');
function lineWith(fragment: string): number {
  const idx = LINES.findIndex((l) => l.includes(fragment));
  assert.ok(idx >= 0, `fixture must contain ${fragment}`);
  return idx + 1;
}

const MISSED = 'payloadThatIsNotInTheFile';

/** Each anchor: its kind, the locator text and the hint line sent with it. */
const ANCHORS: Record<string, { kind: string; text: string; line: number }> = {
  // The model's text is not in the file, with the hint line on the
  // registration, on the parse, and on the tuple the handler builds.
  atRegistration: { kind: 'request_body', text: MISSED, line: lineWith("app.post('/wrapped-parsed'") },
  atParse: { kind: 'request_body', text: MISSED, line: lineWith('OrderBody.parse(request.body)') },
  atTuple: { kind: 'request_body', text: MISSED, line: lineWith("[item, 'item'] as const") },
  directAtRegistration: { kind: 'request_body', text: MISSED, line: lineWith("app.post('/direct-cast'") },
  // Controls: a text that is found binds; another kind keeps its line read.
  foundText: { kind: 'request_body', text: 'request.body as { title: string }', line: lineWith('const typed =') },
  otherKind: { kind: 'response_body', text: MISSED, line: lineWith('return values.length') },
};

describe('carrick#4709: a request body whose text missed is not read off its line', () => {
  let repoDir: string;
  let outRoot: string;
  let result: CaptureStubResult;
  let surface: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4709-capture-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4709-capture-stub-'));
    fs.mkdirSync(path.join(repoDir, 'src'));
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
    fs.writeFileSync(path.join(repoDir, 'src', 'schema.ts'), SCHEMA_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'routes.ts'), ROUTES_TS);

    result = captureStub({
      repoRoot: repoDir,
      serviceName: 'missed-text-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: Object.entries(ANCHORS).map(([name, { kind, text, line }]) => ({
        kind: 'infer' as const,
        alias: `Capture_${name}`,
        source_file: 'src/routes.ts',
        anchor_origin: 'deterministic-infer' as const,
        expression_text: text,
        line_number: line,
        infer_kind: kind,
      })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  function captured(name: string): { text: string; cause: string | undefined } {
    const alias = `Capture_${name}`;
    const record = result.aliases.find((a) => a.alias === alias);
    assert.ok(record, `no capture record for ${alias}`);
    const m = surface.match(
      new RegExp(`export type ${alias} = ([\\s\\S]*?);(?=\\s*(?:export type |$))`)
    );
    assert.ok(m, `no surface line for ${alias}:\n${surface}`);
    return { text: m![1].replace(/\s+/g, ' ').trim(), cause: record!.unread_cause };
  }

  for (const name of ['atRegistration', 'atParse', 'atTuple', 'directAtRegistration']) {
    it(`a missed request text with its hint line ${name} is unknown, not_located`, () => {
      assert.deepStrictEqual(captured(name), { text: 'unknown', cause: 'not_located' });
    });
  }

  it('control: a request text that is found still binds', () => {
    assert.strictEqual(captured('foundText').text, '{ title: string; }');
  });

  it("control: another kind's missed text keeps its line reading", () => {
    assert.strictEqual(captured('otherKind').text, 'number');
  });
});
