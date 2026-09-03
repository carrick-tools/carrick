/**
 * Regression for carrick#631 (producer half): a route handler that returns
 * through a `json()` / `Response`-style helper collapsed to `unknown` or `any`
 * in the type manifest, so the index reported that the endpoint had no
 * contract at all.
 *
 * Two shapes, one mechanism.
 *
 *  (a) The handler returns `wrap(request, reply({ items, total }))`. `wrap` is
 *      local and returns transport machinery (`Response`), so the #371
 *      fail-closed guard abstains and the manifest entry stays `unknown`. The
 *      contract is one level in, in the helper's ARGUMENT.
 *
 *  (b) The handler returns `reply({ … } satisfies ItemListBody)`. The helper
 *      comes from a package with no installed declaration (the scanner reads a
 *      bare checkout), so the helper's own return type is `any` and the whole
 *      response resolved to `any` — even though the contract is written out in
 *      source on the argument.
 *
 * The fix reads the helper's argument when the resolved return carries no
 * contract: it walks the handler's own `return` statements, descends through
 * wrapper calls to the argument that carries a payload, skips branches whose
 * sibling options object states a >= 400 status, and takes the `satisfies`/`as`
 * annotation when the source states one. It is structural throughout — no
 * framework, package or helper name is matched anywhere.
 *
 * How much of the argument is trusted depends on WHY the return carries
 * nothing. Machinery means the callee is known transport, so its argument is
 * the payload. `any` means the callee was merely unresolvable, which on a bare
 * checkout is true of most imported callees, so there only an argument the
 * source annotates counts.
 *
 * Two guards keep the recovery from over-reaching: a returned redirect-style
 * helper whose argument is a bare string must stay unresolved rather than
 * report the path literal, and an unresolvable callee handed an unannotated
 * object (a database query) must stay unresolved rather than report the query.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ROUTE_TS = `import { reply, store } from "framework-runtime-absent";
import { type RemoteListBody } from "shared-contracts-absent";

interface Item {
  id: string;
  label: string;
}

interface ItemListBody {
  items: Item[];
  total: number;
}

declare function listItems(): Item[];
declare function redirectTo(location: string): Response;

/** Local transport wrapper: resolves to machinery, never a contract. */
export async function wrap(request: Request, response: Response): Promise<Response> {
  return request.method === "HEAD" ? new Response(null) : response;
}

export async function loadItems({ request }: { request: Request }) {
  if (request.method.toUpperCase() === "OPTIONS") {
    return wrap(request, reply({}));
  }

  if (!request.url) {
    return wrap(request, reply({ error: "Invalid request parameters" }, { status: 400 }));
  }

  const items = listItems();
  return wrap(request, reply({ items, total: items.length }));
}

export async function loadItemDetail({ request }: { request: Request }) {
  if (!request.url) {
    return reply({ error: "Not found" }, { status: 404 });
  }

  return reply({
    items: listItems(),
    total: 1,
  } satisfies ItemListBody);
}

export async function loadRemoteList({ request }: { request: Request }) {
  if (!request.url) {
    return reply({ error: "Internal Server Error" }, { status: 500 });
  }

  return reply({ items: [], total: 0 } satisfies RemoteListBody);
}

export async function loadRedirect({ request }: { request: Request }) {
  return wrap(request, redirectTo("/somewhere-else"));
}

export async function loadBareStatus({ request }: { request: Request }) {
  if (!request.url) {
    return wrap(request, reply({ error: "Gone" }, 410));
  }

  return wrap(request, reply({ items: listItems(), total: 1 }));
}

export async function loadUntypedQuery({ request }: { request: Request }) {
  return store.findMany({ where: { url: request.url }, take: 20 });
}
`;

function lineOf(marker: string): number {
  const idx = ROUTE_TS.split('\n').findIndex((l) => l.includes(marker));
  assert.ok(idx >= 0, `fixture must contain: ${marker}`);
  return idx + 1;
}

const ITEMS_LINE = lineOf('export async function loadItems');
const DETAIL_LINE = lineOf('export async function loadItemDetail');
const REMOTE_LINE = lineOf('export async function loadRemoteList');
const REDIRECT_LINE = lineOf('export async function loadRedirect');
const BARE_STATUS_LINE = lineOf('export async function loadBareStatus');
const UNTYPED_QUERY_LINE = lineOf('export async function loadUntypedQuery');

interface InferShape {
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    primary_type_symbol?: string;
  }>;
  errors?: string[];
}

/** Whitespace-collapsed comparison — the printer's spacing is not the contract. */
function collapse(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

describe('carrick#631 response helper argument recovery', () => {
  let repoDir: string;
  let client: SidecarClient;
  let routePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-631-'));
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

  async function inferReturn(alias: string, line: number, extractionConfig?: unknown) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: routePath,
          line_number: line,
          infer_kind: 'function_return',
          alias,
        },
      ],
      ...(extractionConfig ? { extraction_config: extractionConfig } : {}),
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('recovers the payload wrapped in a machinery-returning helper', async () => {
    const inferred = await inferReturn('Endpoint_Items_Response', ITEMS_LINE);
    assert.ok(inferred, 'must resolve a response contract, not abstain to unknown');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ items: { id: string; label: string; }[]; total: number; }'
    );
    // Inferred from an object literal, not stated in source.
    assert.strictEqual(inferred.is_explicit, false);
  });

  it('skips the >= 400 status branches and the empty preflight payload', async () => {
    const inferred = await inferReturn('Endpoint_Items_Response_2', ITEMS_LINE);
    assert.ok(inferred);
    assert.ok(
      !/error\s*:/.test(inferred.type_string),
      `error-status branches must not join the contract, got: ${inferred.type_string}`
    );
    assert.ok(
      !/^\{\s*\}\s*\|/.test(collapse(inferred.type_string)),
      `the empty preflight payload must not join the contract, got: ${inferred.type_string}`
    );
  });

  it('takes the `satisfies` annotation when the source states the contract', async () => {
    const inferred = await inferReturn('Endpoint_Detail_Response', DETAIL_LINE);
    assert.ok(inferred, 'must resolve a response contract, not `any`');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ items: { id: string; label: string; }[]; total: number; }'
    );
    assert.strictEqual(inferred.is_explicit, true);
    assert.strictEqual(inferred.primary_type_symbol, 'ItemListBody');
  });

  it('names a stated contract whose declaration is not installed', async () => {
    // Bare checkout: the annotated type has no resolvable declaration, so the
    // structural printer cannot expand it. The source still STATES the
    // contract, so the name reaches the manifest instead of `any`.
    const inferred = await inferReturn('Endpoint_Remote_Response', REMOTE_LINE);
    assert.ok(inferred, 'must resolve to the stated name, not `any`');
    assert.strictEqual(collapse(inferred.type_string), 'RemoteListBody');
    assert.strictEqual(inferred.is_explicit, true);
  });

  it('skips a branch whose status is passed as a bare status code', async () => {
    const inferred = await inferReturn('Endpoint_Bare_Response', BARE_STATUS_LINE);
    assert.ok(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ items: { id: string; label: string; }[]; total: number; }'
    );
  });

  it('does not read an unresolvable callee\'s unannotated argument', async () => {
    // `store` has no installed declaration, so the return resolves to `any` —
    // which says the callee was unresolvable, NOT that it was a response
    // helper. Its argument is a query, and reporting it would be a false
    // contract, worse than the honest `any` the manifest downgrades to Unknown.
    const inferred = await inferReturn('Endpoint_Query_Response', UNTYPED_QUERY_LINE);
    if (inferred) {
      assert.ok(
        !/where|take/.test(inferred.type_string),
        `a query argument is not a response contract, got: ${inferred.type_string}`
      );
      assert.strictEqual(collapse(inferred.type_string), 'any');
    }
  });

  it('recovers the payload when a wrapper rule verified the transport type', async () => {
    // The live shape the offline fixtures missed. Every scan carries an
    // agent-generated extraction config, and a rule that names the transport
    // type and gates it on its origin matches this handler's awaited return,
    // finds no payload inside it (transport carries none), and collapses to
    // `unknown` — which counted as a successful unwrap and routed the whole
    // recovery below out of reach. A verified-machinery collapse is the same
    // verdict as the structural machinery check, so the recovery must still
    // run and read the argument the handler handed the helper.
    const inferred = await inferReturn('Endpoint_Ruled_Response', ITEMS_LINE, {
      rules: [
        {
          wrapperSymbols: ['Response'],
          machineryIndicators: ['headers', 'status', 'statusText', 'ok', 'body'],
          originModuleGlobs: ['typescript/lib/*'],
          unwrapRecursively: false,
        },
      ],
    });
    assert.ok(inferred, 'must resolve a response contract, not abstain to unknown');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ items: { id: string; label: string; }[]; total: number; }'
    );
    // The wrapper's own symbol must never anchor the contract.
    assert.notStrictEqual(inferred.primary_type_symbol, 'Response');
  });

  it('does not report a redirect location as the response contract', async () => {
    const inferred = await inferReturn('Endpoint_Redirect_Response', REDIRECT_LINE);
    if (inferred) {
      assert.ok(
        !/somewhere-else/.test(inferred.type_string) &&
          !/^string$/.test(collapse(inferred.type_string)),
        `a bare string argument is not a contract, got: ${inferred.type_string}`
      );
    } else {
      assert.ok(true, 'abstained (no contract recovered)');
    }
  });
});
