/**
 * carrick#2364: a registration call passed more than one function names no
 * handler.
 *
 * A route helper can take a descriptor callback beside the handler
 * (`app.delete(path, wrap(app, () => ({ ... }), async (req) => { ... }))`).
 * The response was read from the first function on the line, the descriptor,
 * so the route published the descriptor's object as its response contract and
 * every caller was compared against it. Nothing in the source states which of
 * the two functions answers the route, and no argument position or name is
 * taken to decide it: the response is a decided abstain (`unknown` with the
 * root reason `handler_unproven`), so the capture does not re-read it either.
 *
 * A registration passed one function keeps its reading, wrapped or not, and a
 * locator naming an expression inside the handler keeps the containing
 * function's return.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ROUTES_TS = `interface Removed { removed: boolean }
interface Incoming { params: { id: string } }
declare const app: {
  delete(path: string, handler: (request: Incoming) => unknown): void;
};
declare function describedRoute<T>(
  app: unknown,
  describe: () => { area: string; step: string },
  handler: (request: Incoming) => Promise<T>
): (request: Incoming) => Promise<T>;
declare function wrapped<T>(
  handler: (request: Incoming) => Promise<T>
): (request: Incoming) => Promise<T>;

app.delete('/described/:id', describedRoute(app, () => ({ area: 'items', step: 'remove' }), async (request) => {
  const removed: Removed = { removed: true }
  return removed
}))

app.delete('/wrapped/:id', wrapped(async (request) => {
  const removed: Removed = { removed: true }
  return removed
}))

app.delete('/plain/:id', async (request) => {
  const removed: Removed = { removed: true }
  return removed
})
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: Array<{ path: string; reason: string }>;
  }>;
}

const lineOf = (needle: string): number => {
  const at = ROUTES_TS.indexOf(needle);
  assert.ok(at >= 0, `fixture must contain ${needle}`);
  return ROUTES_TS.slice(0, at).split('\n').length;
};

const REMOVED = '{ removed: boolean; }';

describe('carrick#2364: a registration passed several functions names no handler', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2364-'));
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
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, ROUTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  let seq = 0;
  const infer = async (request: Record<string, unknown>) => {
    seq += 1;
    const alias = `Response_${seq}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [{ file_path: routesPath, alias, ...request }],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  const assertUnproven = (row: Awaited<ReturnType<typeof infer>>, how: string) => {
    assert.ok(row, `${how}: the row must be answered, as a decision`);
    assert.strictEqual(row.type_string, 'unknown', `${how}: no response is read`);
    assert.ok(
      row.any_provenance?.some((p) => p.path === '' && p.reason === 'handler_unproven'),
      `${how}: the abstain carries its root reason, got ${JSON.stringify(row.any_provenance)}`
    );
  };

  const described = lineOf("app.delete('/described");
  const describedReturn = lineOf('return removed');

  for (const infer_kind of ['response_body', 'function_return']) {
    it(`${infer_kind} at the registration line abstains`, async () => {
      assertUnproven(await infer({ line_number: described, infer_kind }), infer_kind);
    });
  }

  it('response_body on the helper call span abstains', async () => {
    const start = ROUTES_TS.indexOf('describedRoute(app');
    const end = ROUTES_TS.indexOf('}))', start) + 2;
    assertUnproven(
      await infer({ line_number: described, span_start: start, span_end: end, infer_kind: 'response_body' }),
      'helper span'
    );
  });

  it('response_body following the registration from a returned expression abstains', async () => {
    assertUnproven(
      await infer({
        line_number: described,
        expression_text: 'removed',
        expression_line: describedReturn,
        infer_kind: 'response_body',
      }),
      'returned expression'
    );
  });

  it('a function_return locator inside the handler keeps that function', async () => {
    const row = await infer({
      line_number: described,
      expression_text: 'removed',
      expression_line: describedReturn,
      infer_kind: 'function_return',
    });
    assert.strictEqual(row?.type_string, REMOVED);
  });

  for (const route of ['/wrapped', '/plain']) {
    for (const infer_kind of ['response_body', 'function_return']) {
      it(`${route}: one function passed keeps its ${infer_kind} reading`, async () => {
        const row = await infer({ line_number: lineOf(`app.delete('${route}`), infer_kind });
        assert.strictEqual(row?.type_string, REMOVED);
      });
    }
  }
});
