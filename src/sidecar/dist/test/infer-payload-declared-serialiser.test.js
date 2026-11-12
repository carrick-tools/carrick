/**
 * carrick#2472 S7 (blueprint row T7): proof for the replacement of the deleted
 * sibling-call rule.
 *
 * A file-based handler returns `serialise(payload, { status })`. The old rule
 * promoted the callee to "serialiser" from a returned sibling call that stated
 * a status (deleted in #3019). The replacement is the compiler: when the
 * callee has a resolvable declaration returning `Response`, the return is
 * transport machinery and the walk reads the handler's returned argument, so
 * the payload type is the declared one, not `any`.
 *
 * The control handler is byte-identical except that its callee comes from a
 * module with no declaration. It stays at the honest `any`
 * (`no_payload_evidence`). Together they show the result comes from the
 * declaration alone, not from the sibling call.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const TRANSPORT_TS = `export declare function serialise(
  body: unknown,
  init?: number | { status?: number }
): Response;
`;
const HANDLER = (callee, name) => `
export async function ${name}({ request }: { request: Request }) {
  if (!request.url) {
    return ${callee}({ error: "Invalid or missing credential" }, { status: 401 });
  }

  const claims: Detail = {
    id: request.method,
    label: "only",
  };

  return ${callee}(claims);
}
`;
const ROUTE_TS = `import { serialise } from "./transport";
import { makeForeign } from "./factory";
import { serialise as undeclared } from "transport-runtime-absent";

interface Detail {
  id: string;
  label: string;
}
${HANDLER('serialise', 'declaredCallee')}${HANDLER('undeclared', 'undeclaredCallee')}
export async function foreignTypeCallee({ request }: { request: Request }) {
  if (!request.url) {
    return undeclared({ error: "missing" }, { status: 400 });
  }
  const foreign = makeForeign();
  return undeclared(foreign);
}
`;
// The route imports the factory but not the module that declares the type, so
// the compiler can only name that type by where its module sits on disk.
const FACTORY_TS = `import type { ForeignRecord } from "./foreign-model";
export declare function makeForeign(): Map<string, ForeignRecord>;
`;
const FOREIGN_MODEL_TS = `export class ForeignRecord {
  private secret = 1;
  ref = "";
  count = 0;
}
`;
function lineOf(marker) {
    const idx = ROUTE_TS.split('\n').findIndex((l) => l.includes(marker));
    assert.ok(idx >= 0, `fixture must contain: ${marker}`);
    return idx + 1;
}
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick#2472 T7: declared serialiser callee', () => {
    let repoDir;
    let client;
    let routePath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2472-t7-'));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
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
        }));
        fs.writeFileSync(path.join(repoDir, 'src', 'transport.ts'), TRANSPORT_TS);
        fs.writeFileSync(path.join(repoDir, 'src', 'factory.ts'), FACTORY_TS);
        fs.writeFileSync(path.join(repoDir, 'src', 'foreign-model.ts'), FOREIGN_MODEL_TS);
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
    async function inferReturn(alias, marker) {
        const res = await client.send({
            action: 'infer',
            request_id: alias,
            requests: [
                { file_path: routePath, line_number: lineOf(marker), infer_kind: 'function_return', alias },
            ],
        });
        return (res.inferred_types ?? []).find((t) => t.alias === alias);
    }
    // The model's response locator: `claims`, the argument of the handler's
    // success return, asked at the line that declares the handler (carrick#807).
    async function inferLocated(alias, marker, returnMarker, expression = 'claims') {
        const res = await client.send({
            action: 'infer',
            request_id: alias,
            requests: [
                {
                    file_path: routePath,
                    line_number: lineOf(marker),
                    infer_kind: 'response_body',
                    alias,
                    expression_text: expression,
                    expression_line: lineOf(returnMarker),
                },
            ],
        });
        return (res.inferred_types ?? []).find((t) => t.alias === alias);
    }
    it('reads the payload from the returned argument when the callee declares a Response', async () => {
        const inferred = await inferReturn('Declared_Response', 'function declaredCallee');
        assert.ok(inferred, 'must resolve a contract, not abstain');
        assert.notStrictEqual(collapse(inferred.type_string), 'any');
        assert.strictEqual(collapse(inferred.type_string), '{ id: string; label: string; }');
        assert.ok(!/error/.test(inferred.type_string), 'the 401 branch is not the contract');
    });
    it('control: the same handler with an undeclared callee stays at the honest any', async () => {
        const inferred = await inferReturn('Undeclared_Response', 'function undeclaredCallee');
        assert.ok(inferred);
        assert.strictEqual(collapse(inferred.type_string), 'any');
        assert.strictEqual((inferred.any_provenance ?? [])[0]?.reason, 'no_payload_evidence');
    });
    // Blueprint row N34: where the returned callee resolves to nothing, the
    // model's locator alone says the call is a response sender. That is a lead,
    // never a type the check enforces: the row stays at the honest `any`, and
    // the lead rides in its provenance.
    it('an unresolved callee: the model-located body is a lead in the provenance, not the type', async () => {
        const inferred = await inferLocated('Undeclared_Located', 'function undeclaredCallee', 'return undeclared(claims)');
        assert.ok(inferred, 'the row keeps its honest any');
        assert.strictEqual(collapse(inferred.type_string), 'any');
        const lead = (inferred.any_provenance ?? []).find((p) => p.reason === 'no_payload_evidence');
        assert.ok(lead, JSON.stringify(inferred.any_provenance));
        assert.match(lead.detail ?? '', /model located/);
        assert.match(lead.detail ?? '', /Detail|id: string/);
    });
    it('an unresolved callee: a lead whose type comes from an unimported module names no path', async () => {
        const inferred = await inferLocated('Foreign_Located', 'function foreignTypeCallee', 'return undeclared(foreign)', 'foreign');
        assert.ok(inferred, 'the row keeps its honest any');
        const lead = (inferred.any_provenance ?? []).find((p) => p.reason === 'no_payload_evidence');
        assert.ok(lead, JSON.stringify(inferred.any_provenance));
        const detail = lead.detail ?? '';
        assert.match(detail, /model located/);
        assert.match(detail, /ForeignRecord/);
        assert.doesNotMatch(detail, /import\("/);
        assert.doesNotMatch(detail, /\/(?:tmp|var|private|Users|home)\//);
        assert.ok(!detail.includes(repoDir), detail);
    });
    it('a declared callee: the located body is still the type', async () => {
        const inferred = await inferLocated('Declared_Located', 'function declaredCallee', 'return serialise(claims)');
        assert.ok(inferred);
        assert.strictEqual(collapse(inferred.type_string), '{ id: string; label: string; }');
    });
});
