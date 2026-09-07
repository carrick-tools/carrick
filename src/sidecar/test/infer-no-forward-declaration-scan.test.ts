/**
 * Regression for carrick#766: a line-only anchor that lands on a statement
 * which declares no function was answered by the NEXT function declaration in
 * the file.
 *
 * `findFunctionByLine` accepted any function whose declaration starts within
 * two lines of the anchor, in either direction. A route whose handlers are
 * re-exported at the bottom of the file
 *
 *     export { entry, read };            // the anchor line
 *
 *     function toRouteFilters(raw) { … } // two lines later
 *
 * therefore reported the private helper's return type as the endpoint's
 * response contract. Silently: nothing about the answer says it came from a
 * neighbouring declaration, so a helper with a clean return type reads as the
 * contract with no signal at all. A confidently wrong contract is worse for a
 * consumer check than the honest `unknown`, which is what this now answers.
 *
 * The tolerance itself is load-bearing — an anchor often names the line of a
 * registration or a binding whose handler starts a line or two in — so it is
 * kept, with the one rule that makes it safe: a candidate starting AFTER the
 * anchor line is rejected when a statement begins at or after the anchor line
 * and does not contain it. Nothing but trivia may sit between the anchor and
 * the function it is taken to name.
 *
 * Resolving the exported binding to its registration's handler (which would
 * answer `{ id: string }` here rather than abstaining) needs to know WHICH of
 * the two exported bindings the operation is — the export names both an action
 * and a loader — and the infer request does not carry that. Tracked separately;
 * this change is the half that stops the false contract.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ROUTE_TS = `interface RouteFilters {
  channels?: string[];
  windowMs?: number;
}

interface CreatedBody {
  id: string;
}

interface OkBody {
  ok: boolean;
}

interface NameBody {
  name: string;
}

declare function reply(body: unknown, init?: { status: number }): Response;

declare function buildRoute<T>(
  config: { schema: string },
  handler: (ctx: { body: T }) => Promise<Response>
): { entry: (ctx: { body: T }) => Promise<Response> };

const { entry } = buildRoute({ schema: "create" }, async () => {
  return reply({ id: "bulk_1" } satisfies CreatedBody);
});

const read = buildRoute({ schema: "list" }, async () => {
  return reply({ id: "bulk_2" } satisfies CreatedBody);
});

export { entry, read };

function toRouteFilters(raw: string): RouteFilters {
  return { channels: [raw], windowMs: 60 };
}

export const wrapped = buildRoute(
  { schema: "ok" },
  async () => {
    return reply({ ok: true } satisfies OkBody);
  }
);

export async function loadName() {
  return reply({ name: toRouteFilters("x").channels?.[0] ?? "" } satisfies NameBody);
}
`;

function lineOf(marker: string): number {
  const idx = ROUTE_TS.split('\n').findIndex((l) => l.includes(marker));
  assert.ok(idx >= 0, `fixture must contain: ${marker}`);
  return idx + 1;
}

const EXPORT_LINE = lineOf('export { entry, read };');
const HELPER_LINE = lineOf('function toRouteFilters');
const WRAPPED_LINE = lineOf('export const wrapped = buildRoute(');
const NAME_BODY_LINE = lineOf('satisfies NameBody');

interface InferShape {
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
  }>;
  errors?: string[];
}

describe('carrick#766 line anchors never read the next declaration', () => {
  let repoDir: string;
  let client: SidecarClient;
  let routePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-766-'));
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
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    routePath = path.join(repoDir, 'src', 'route.ts');
    fs.writeFileSync(routePath, ROUTE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function infer(alias: string, line: number, kind: string) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        { file_path: routePath, line_number: line, infer_kind: kind, alias },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('the fixture really does put the helper inside the old tolerance', () => {
    assert.strictEqual(
      HELPER_LINE - EXPORT_LINE,
      2,
      'the helper must sit exactly at the tolerance edge, or this proves nothing'
    );
  });

  it('does not report the next declaration as a re-exported route response', async () => {
    const inferred = await infer('Endpoint_Export_Response', EXPORT_LINE, 'response_body');
    const text = inferred?.type_string ?? '';
    assert.ok(
      !/channels|windowMs/.test(text),
      `the private helper's contract must never be the route's, got: ${text}`
    );
  });

  it('does not report the next declaration for a function_return anchor either', async () => {
    const inferred = await infer('Endpoint_Export_Return', EXPORT_LINE, 'function_return');
    const text = inferred?.type_string ?? '';
    assert.ok(
      !/channels|windowMs/.test(text),
      `the private helper's contract must never be the route's, got: ${text}`
    );
  });

  it('still follows a handler the anchor line\'s own statement contains', async () => {
    // The arrow starts a line after the anchor, so the tolerance is what finds
    // it — and the statement at the anchor line CONTAINS it, so the guard has
    // nothing to separate. This is the case the tolerance exists for.
    const inferred = await infer('Endpoint_Wrapped_Return', WRAPPED_LINE, 'function_return');
    assert.ok(inferred, 'a contained handler must still resolve');
    assert.match(inferred.type_string, /ok\s*:\s*boolean/);
  });

  it('still resolves the function an anchor line sits inside', async () => {
    // The anchor is INSIDE the function, so the candidate starts before it and
    // the guard never applies.
    const inferred = await infer('Endpoint_Name_Return', NAME_BODY_LINE, 'function_return');
    assert.ok(inferred, 'an enclosing function must still resolve');
    assert.match(inferred.type_string, /name\s*:\s*string/);
  });
});
