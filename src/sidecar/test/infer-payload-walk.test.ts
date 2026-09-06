/**
 * carrick#707: the payload-argument walk for file-based route handlers.
 *
 * A file-based route module exports a handler and never annotates its return.
 * The payload either goes straight back or is handed to a serialiser, and the
 * serialiser's result is transport. carrick#631 already reads the serialiser's
 * ARGUMENT when the resolved return carries no contract, but only in two
 * situations: the callee resolves to known transport machinery, or the source
 * annotates the argument. On a bare checkout — what CI scans — neither holds
 * for the common shapes, so the whole endpoint reports `any`.
 *
 * Two gaps, measured on the trig-bench mirror before this change:
 *
 *  (A) `return serialise(payload)` where `serialise` has no installed
 *      declaration. The return is `any`, which says the callee was
 *      unresolvable, not that it was a serialiser — so the recovery stays
 *      restricted to a stated annotation and the unannotated payload is left
 *      behind. But the handler itself states what the callee is: a SIBLING
 *      returned call hands the same callee a body plus an argument carrying an
 *      HTTP status. A body-plus-status call is the structural signature of a
 *      response serialiser, written in the handler's own source, so it is
 *      evidence available on a bare checkout and it promotes the unannotated
 *      argument to a payload.
 *
 *  (B) The success payload sits inside a callback the handler hands to the
 *      returned call (`return settle(...).match(ok => serialise(body), err =>
 *      …)`). The walk only reads the handler's own `return` statements, so it
 *      never sees it.
 *
 * Everything here is structural: no framework, package, helper or method name
 * is matched anywhere, and the evidence for (A) comes from the handler's own
 * returned expressions rather than from anything the walk knows about HTTP.
 *
 * The negatives are the point of the design, not decoration:
 *  - a status-stating argument in FIRST position is a payload with a status
 *    field, not a body-plus-status call, and must not promote its callee;
 *  - evidence from a call the handler does not RETURN (a logger) must not
 *    promote its callee;
 *  - a callback return that is not itself a serialiser call must not be read
 *    as the payload, or `return rows.map(r => ({ … }))` reports a row as the
 *    endpoint's response;
 *  - an unresolvable callee with no evidence at all still reports `any`, and
 *    now says why.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ROUTE_TS = `import { serialise, archive, settle, audit, rows, run } from "transport-runtime-absent";

interface Detail {
  id: string;
  label: string;
}

/** Local transport wrapper: resolves to machinery, never a contract. */
export async function wrapTransport(request: Request, response: Response): Promise<Response> {
  return request.method === "HEAD" ? new Response(null) : response;
}

export async function statusSiblingProvesSerialiser({ request }: { request: Request }) {
  if (!request.url) {
    return serialise({ error: "Invalid or missing credential" }, { status: 401 });
  }

  const claims = {
    sub: request.method,
    pub: true,
  };

  return serialise(claims);
}

export async function bareStatusSiblingProvesSerialiser({ request }: { request: Request }) {
  if (!request.url) {
    return serialise({ error: "Gone" }, 410);
  }

  return serialise({ id: "1", label: "only" });
}

export async function noEvidenceStaysUnresolved({ request }: { request: Request }) {
  return archive({ where: { url: request.url }, take: 20 });
}

export async function statusInFirstArgumentProvesNothing({ request }: { request: Request }) {
  if (!request.url) {
    return settle({ status: 503, note: "unavailable" });
  }

  return settle({ id: "1", label: "only" });
}

export async function evidenceMustComeFromAReturnedCall({ request }: { request: Request }) {
  audit({ event: "served" }, { status: 500 });
  return audit({ id: "1", label: "only" });
}

export async function payloadInsideACallback({ request }: { request: Request }) {
  if (!request.url) {
    return serialise({ error: "Invalid request body" }, { status: 400 });
  }

  return await run(request.url).match(
    (result) => {
      return serialise(
        {
          id: result.id,
          label: result.label,
        } satisfies Detail,
        { status: 201 }
      );
    },
    (error) => {
      return serialise({ error: error.message }, { status: 500 });
    }
  );
}

export async function callbackReturnIsNotItselfAPayload({ request }: { request: Request }) {
  return rows(request.url).map((row) => ({ id: row.id, label: row.label }));
}

export async function machineryCallbackReturnIsNotAPayload({ request }: { request: Request }) {
  return wrapTransport(
    request,
    rows(request.url).map((row) => ({ id: row.id, label: row.label }))
  );
}
`;

function lineOf(marker: string): number {
  const idx = ROUTE_TS.split('\n').findIndex((l) => l.includes(marker));
  assert.ok(idx >= 0, `fixture must contain: ${marker}`);
  return idx + 1;
}

const LINES = {
  statusSibling: lineOf('export async function statusSiblingProvesSerialiser'),
  bareStatusSibling: lineOf('export async function bareStatusSiblingProvesSerialiser'),
  noEvidence: lineOf('export async function noEvidenceStaysUnresolved'),
  statusFirstArg: lineOf('export async function statusInFirstArgumentProvesNothing'),
  notReturned: lineOf('export async function evidenceMustComeFromAReturnedCall'),
  callbackPayload: lineOf('export async function payloadInsideACallback'),
  callbackRow: lineOf('export async function callbackReturnIsNotItselfAPayload'),
  machineryCallbackRow: lineOf('export async function machineryCallbackReturnIsNotAPayload'),
};

interface ProvenanceShape {
  path: string;
  kind: string;
  reason: string;
  detail?: string;
}

interface InferShape {
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    primary_type_symbol?: string;
    any_provenance?: ProvenanceShape[];
  }>;
  errors?: string[];
}

function collapse(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

describe('carrick#707 payload-argument walk', () => {
  let repoDir: string;
  let client: SidecarClient;
  let routePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-707-'));
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

  async function inferReturn(alias: string, line: number) {
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
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('reads the unannotated payload when a returned sibling states a status', async () => {
    const inferred = await inferReturn('Sibling_Response', LINES.statusSibling);
    assert.ok(inferred, 'must resolve a response contract, not `any`');
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ sub: string; pub: boolean; }'
    );
    assert.strictEqual(inferred.is_explicit, false);
  });

  it('accepts a bare status code as the same evidence', async () => {
    const inferred = await inferReturn('BareSibling_Response', LINES.bareStatusSibling);
    assert.ok(inferred, 'must resolve a response contract, not `any`');
    assert.strictEqual(collapse(inferred.type_string), '{ id: string; label: string; }');
  });

  it('leaves an unresolvable callee with no evidence at `any`, and says why', async () => {
    const inferred = await inferReturn('NoEvidence_Response', LINES.noEvidence);
    assert.ok(inferred, 'the honest `any` is still reported');
    assert.ok(
      !/where|take/.test(inferred.type_string),
      `a query argument is not a response contract, got: ${inferred.type_string}`
    );
    assert.strictEqual(collapse(inferred.type_string), 'any');
    const provenance = inferred.any_provenance ?? [];
    assert.strictEqual(provenance.length, 1, `expected one reason, got ${JSON.stringify(provenance)}`);
    assert.strictEqual(provenance[0].path, '');
    assert.strictEqual(provenance[0].kind, 'any');
    assert.strictEqual(provenance[0].reason, 'no_payload_evidence');
  });

  it('does not read a status field in first position as evidence', async () => {
    const inferred = await inferReturn('FirstArg_Response', LINES.statusFirstArg);
    assert.ok(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      'any',
      'a payload whose own body carries a status does not make its callee a serialiser'
    );
  });

  it('does not read evidence from a call the handler never returns', async () => {
    const inferred = await inferReturn('NotReturned_Response', LINES.notReturned);
    assert.ok(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      'any',
      'a status on a non-returned call says nothing about what the handler serialises'
    );
  });

  it('recovers a stated payload the handler hands to a callback', async () => {
    const inferred = await inferReturn('Callback_Response', LINES.callbackPayload);
    assert.ok(inferred, 'must resolve a response contract, not abstain');
    assert.strictEqual(collapse(inferred.type_string), '{ id: string; label: string; }');
    assert.strictEqual(inferred.is_explicit, true);
    assert.strictEqual(inferred.primary_type_symbol, 'Detail');
  });

  it('does not read a callback return that is not itself a serialiser call', async () => {
    const inferred = await inferReturn('CallbackRow_Response', LINES.callbackRow);
    assert.ok(inferred);
    assert.ok(
      !/label/.test(inferred.type_string),
      `a mapped row is not a response contract, got: ${inferred.type_string}`
    );
  });

  it('does not read a mapped row through a machinery wrapper either', async () => {
    const inferred = await inferReturn('MachineryRow_Response', LINES.machineryCallbackRow);
    if (inferred) {
      assert.ok(
        !/label/.test(inferred.type_string),
        `a mapped row is not a response contract, got: ${inferred.type_string}`
      );
    }
  });
});
