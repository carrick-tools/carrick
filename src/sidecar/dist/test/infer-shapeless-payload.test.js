/**
 * carrick#2116: a narrowed `unknown` and `object` state no shape.
 *
 * A handler that returns a value it narrowed from `unknown` (`if (cached !=
 * null) return cached`) has the return type `{}`: the checker's own empty
 * object type, which also swallows a typed branch beside it. A request cast
 * `as object` is `object`. Both were published as written and judged as a
 * shape that lacks every field the other side names, so the pair read
 * incompatible on a type that states nothing.
 *
 * The printed text cannot decide this. An empty object a handler really sends
 * (`return {}`) and one its author declares (`(): {}`) print the same two
 * characters, and a consumer that expects fields from them is a true finding.
 * The source program tells them apart: the narrowed `unknown` is the one empty
 * object type with no symbol.
 *
 * Both places a payload type is read off the source program answer `unknown`
 * for it: the inferrer, and the capture's own locator read (which re-reads
 * the same locator whenever the inferrer's answer is not a decided one).
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { runCheck } from '../src/capture/index.js';
const ROUTES_TS = `export interface Marker {}
interface Saved { saved: boolean }
declare function loadCached(): unknown;
declare function compute(): Saved;
declare function parseInput(text: string): unknown;
declare function submit(body: unknown): Promise<void>;

export function narrowed() {
  const cached = loadCached();
  if (cached != null) {
    return cached; // narrowed return
  }
  return compute();
}

export function narrowedOrNothing() {
  const cached = loadCached();
  if (cached != null) return cached;
  return undefined;
}

export function emptyLiteral() {
  return {}; // empty literal
}

export function declaredEmpty(): {} {
  return compute();
}

export function emptyInterface(): Marker {
  return {};
}

export function memberNarrowed() {
  const cached = loadCached();
  if (cached == null) return { meta: {}, saved: true };
  return { meta: cached, saved: true };
}

export async function send(text: string) {
  const input = parseInput(text) as object;
  await submit(input); // request sent
}
`;
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
function lineOf(marker) {
    const index = ROUTES_TS.split('\n').findIndex((line) => line.includes(marker));
    assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
    return index + 1;
}
describe('carrick#2116: a payload that is a narrowed unknown or object states no shape', () => {
    let client;
    let repoDir;
    let outRoot;
    let routesPath;
    before(async () => {
        repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2116-repo-')));
        outRoot = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2116-stub-')));
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                rootDir: 'src',
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                skipLibCheck: true,
            },
            include: ['src'],
        }));
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
        fs.rmSync(outRoot, { recursive: true, force: true });
    });
    const infer = async (alias, kind, at, expression) => {
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: routesPath,
                    line_number: lineOf(at),
                    infer_kind: kind,
                    alias,
                    ...(expression ? { expression_text: expression, expression_line: lineOf(at) } : {}),
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        return inferred ? collapse(inferred.type_string) : undefined;
    };
    describe('the inferrer', () => {
        it("reads a handler's narrowed unknown as unknown, as a return and as a located send", async () => {
            assert.strictEqual(await infer('Narrowed_Return', 'function_return', 'export function narrowed()'), 'unknown');
            assert.strictEqual(await infer('Narrowed_Body', 'response_body', '// narrowed return', 'cached'), 'unknown');
        });
        it('reads a narrowed unknown beside undefined as unknown', async () => {
            assert.strictEqual(await infer('Narrowed_Or_Nothing', 'function_return', 'export function narrowedOrNothing()'), 'unknown');
        });
        it('reads a request cast to object as unknown', async () => {
            assert.strictEqual(await infer('Object_Request', 'request_body', '// request sent', 'input'), 'unknown');
        });
        it('still publishes an empty object the handler writes', async () => {
            assert.strictEqual(await infer('Empty_Literal', 'function_return', 'export function emptyLiteral()'), '{}');
        });
        it('still publishes an empty object the author declares, and an empty interface', async () => {
            assert.strictEqual(await infer('Declared_Empty', 'function_return', 'export function declaredEmpty()'), '{}');
            const marker = await infer('Empty_Interface', 'function_return', 'export function emptyInterface()');
            assert.ok(marker === '{}' || marker === 'Marker', String(marker));
        });
        it('leaves a member that is a narrowed unknown a member', async () => {
            const text = await infer('Member', 'function_return', 'export function memberNarrowed()');
            assert.ok(text !== undefined && text !== 'unknown', String(text));
            assert.match(text, /saved: boolean/);
            assert.match(text, /meta: \{\s*\}/);
        });
    });
    describe('the capture, and the check after it', () => {
        let result;
        let surface;
        const locator = (alias, at, expression) => ({
            kind: 'infer',
            alias,
            source_file: 'src/routes.ts',
            anchor_origin: 'deterministic-infer',
            line_number: lineOf(at),
            ...(expression ? { expression_text: expression } : {}),
        });
        before(async () => {
            const frame = await client.send({
                action: 'capture_v2',
                request_id: 'capture-2116',
                repo_root: repoDir,
                service_name: 'api',
                anchors: [
                    locator('Narrowed_Response', '// narrowed return', 'cached'),
                    locator('Object_Request', '// request sent', 'input'),
                    locator('Literal_Response', '// empty literal', '{}'),
                ],
                out_dir: path.join(outRoot, 'api'),
                scan_root: repoDir,
            }, 120_000);
            assert.strictEqual(frame.status, 'success', JSON.stringify(frame.errors));
            assert.ok(frame.result, 'the capture answered with no result');
            result = frame.result;
            surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
        });
        const aliasText = (alias) => {
            const match = surface.match(new RegExp(`export type ${alias} = ([^;]+);`));
            assert.ok(match, `no surface line for ${alias} in:\n${surface}`);
            return match[1].trim();
        };
        it('publishes unknown for the narrowed unknown and for object', () => {
            assert.strictEqual(aliasText('Narrowed_Response'), 'unknown');
            assert.strictEqual(aliasText('Object_Request'), 'unknown');
        });
        it('still publishes the empty object the handler writes', () => {
            assert.strictEqual(collapse(aliasText('Literal_Response')), '{}');
        });
        it('reads the pair unverifiable for an unknown side, never incompatible', async () => {
            const webDir = path.join(outRoot, 'web');
            fs.mkdirSync(path.join(webDir, 'types'), { recursive: true });
            fs.writeFileSync(path.join(webDir, 'package.json'), JSON.stringify({
                name: '@carrick/web',
                version: '0.0.0-carrick',
                private: true,
                types: './types/surface.d.ts',
            }) + '\n');
            fs.writeFileSync(path.join(webDir, 'types', 'surface.d.ts'), 'export type Expected_Response = { saved: boolean };\n');
            const pair = (pair_key, producerAlias) => ({
                pair_key,
                protocol: 'http',
                type_kind: 'response',
                producer: { service_name: 'api', alias: producerAlias },
                consumer: { service_name: 'web', alias: 'Expected_Response' },
            });
            const check = await runCheck({
                stubs: [
                    { service_name: 'api', stub_dir: result.stub_dir },
                    { service_name: 'web', stub_dir: webDir },
                ],
                pairs: [pair('narrowed', 'Narrowed_Response'), pair('literal', 'Literal_Response')],
            });
            assert.strictEqual(check.success, true, JSON.stringify(check.errors));
            const verdict = (key) => check.verdicts.find((v) => v.pair_key === key);
            assert.strictEqual(verdict('narrowed')?.bucket, 'unverifiable');
            assert.match(verdict('narrowed')?.diagnostic ?? '', /'unknown'/);
            // The control: a handler that really sends `{}` is still judged, and a
            // consumer that expects a field from it is a finding.
            assert.strictEqual(verdict('literal')?.bucket, 'incompatible');
        });
    });
});
