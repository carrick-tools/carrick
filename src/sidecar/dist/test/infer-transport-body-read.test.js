/**
 * carrick#2714, carrick#1841: a consumer call that answers a request
 * library's response object (a status, headers and a request config around
 * the body in `data`) published that object as its expected response, so
 * every producer body read as missing the transport's members.
 *
 * The consumer's expected response is what the code reads the body as:
 *
 *  - the type the source states at the read of the payload member: a cast
 *    (`res.data as Widget`), an annotation (`const w: Widget = res.data`), or
 *    the declared return type of the function that returns it unchanged;
 *  - the payload member's own type where the call states it
 *    (`http.get<Widget>(url)`), as before;
 *  - nothing, decided (`machinery_envelope` at the root), where neither
 *    states it: the body is untyped, or the response object is handed on
 *    whole and its body is read elsewhere. A call whose result type the
 *    compiler took from its return position (`Promise<void>`) still returns
 *    the response object, so it is decided the same way.
 *
 * And where the source casts the body it read to a placeholder union and
 * narrows it later (`json as { data: Widget[] }`), the narrowed statement is
 * the body, reported as stated, so a model symbol naming the element inside
 * it is not kept.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const HTTP_PACKAGE = 'tiny-http';
const HTTP_PACKAGE_DTS = `export interface HttpRequestConfig<D = any> {
  url?: string;
  data?: D;
}
export interface HttpResponse<T = any, D = any> {
  data: T;
  status: number;
  statusText: string;
  headers: Record<string, string>;
  config: HttpRequestConfig<D>;
}
export interface HttpClient {
  get<T = any, R = HttpResponse<T>>(url: string): Promise<R>;
  post<T = any, R = HttpResponse<T>, D = any>(url: string, body?: D): Promise<R>;
  delete<T = any, R = HttpResponse<T>>(url: string): Promise<R>;
}
export declare const http: HttpClient;

declare const responseDefault: unique symbol;
type ResponseDefault = typeof responseDefault;
type ResponseResult<T, R, D> = R extends ResponseDefault ? HttpResponse<T, D> : R;
export interface ResultClient {
  get<T = any, R = ResponseDefault>(url: string): Promise<ResponseResult<T, R, any>>;
  post<T = any, R = ResponseDefault, D = any>(url: string, body?: D): Promise<ResponseResult<T, R, D>>;
}
export declare const results: ResultClient;

type ResponseMap = { raw: HttpResponse<unknown> };
type MappedResult<R, J> = R extends keyof ResponseMap ? ResponseMap[R] : J;
export interface BodyClient {
  fetch<T = any, R extends string = "json">(url: string): Promise<MappedResult<R, T>>;
}
export declare const bodies: BodyClient;

export interface WireResponse {
  ok: boolean;
  status: number;
  headers: Record<string, string>;
  json(): Promise<unknown>;
}
export declare function send(url: string): Promise<WireResponse>;
export declare function echo<R = HttpResponse<any>>(value: R): Promise<R>;
`;
const SERVICE_TS = `import { http, results, bodies, send, echo, type HttpResponse, type WireResponse } from "tiny-http";

export interface Widget {
  id: string;
  label: string;
}

export async function castOnMember() {
  const res = await http.get("/v1/widgets/cast");
  return res.data as Widget;
}

export async function annotationOnMember() {
  const res = await http.get("/v1/widgets/annotated");
  const widget: Widget = res.data;
  return widget.id;
}

export async function declaredReturn(): Promise<Widget> {
  const res = await http.get("/v1/widgets/declared");
  if (!res.data) {
    throw new Error("empty");
  }
  return res.data;
}

export async function unstated() {
  const res = await http.get("/v1/widgets/unstated");
  return res.data.id;
}

export async function returnedAsVoid(): Promise<void> {
  return http.post("/v1/widgets/ping", {});
}

export async function returnedWhole(): Promise<HttpResponse<void>> {
  return await http.delete("/v1/widgets/removed");
}

export async function typedCall() {
  const res = await http.get<Widget>("/v1/widgets/typed");
  return res.data;
}

export async function castThroughResultAlias() {
  const res = await results.get("/v1/widgets/result-cast");
  return res.data as Widget;
}

export async function returnedAsVoidThroughResultAlias(): Promise<void> {
  return results.post("/v1/widgets/result-ping", {});
}

export async function bodyReturned(): Promise<Widget> {
  return bodies.fetch("/v1/widgets/body");
}

async function readJson<T>(res: WireResponse): Promise<T> {
  return (await res.json()) as T;
}

export async function sentAndRead() {
  const res = await send("/v1/widgets/sent");
  if (!res.ok) {
    throw new Error("failed");
  }
  const widget: Widget = await readJson(res);
  return widget;
}

export async function inPlace(): Promise<Widget> {
  return (await http.get("/v1/widgets/in-place")).data;
}

export async function restatedAfterRead() {
  const res = await apiFetch("/v1/widgets/restated");
  const json = (await res.json()) as WidgetPage;
  return json as unknown as Gadget;
}

export async function destructured() {
  const { data } = await http.get("/v1/widgets/destructured");
  return data as Widget;
}

export interface Gadget {
  serial: number;
}

export interface WidgetPage {
  data: Widget[];
}

export async function twoStatements() {
  const res = await http.get("/v1/widgets/two");
  const widget = res.data as Widget;
  const gadget = res.data as Gadget;
  return [widget, gadget];
}

export async function echoed(widget: Widget): Promise<Widget> {
  return echo(widget);
}

// The platform's response: its json() is the body read by declaration (blueprint row N33).
declare function apiFetch(url: string): Promise<Response>;

export async function narrowedEnvelope(): Promise<{ data: Widget[] }> {
  const res = await apiFetch("/v1/widgets");
  const json = (await res.json()) as { data: Widget[] } | { error: unknown };
  if (!res.ok || "error" in json) {
    throw new Error("failed");
  }
  return json as { data: Widget[] };
}
`;
/** The rule a scan receives for the package's response object. */
const EXTRACTION_CONFIG = {
    rules: [
        {
            wrapperSymbols: ['HttpResponse'],
            machineryIndicators: ['status', 'statusText', 'headers', 'config'],
            originModuleGlobs: [HTTP_PACKAGE, `${HTTP_PACKAGE}/*`],
            payloadGenericIndex: 0,
            payloadPropertyPath: ['data'],
            unwrapRecursively: true,
            maxDepth: 4,
        },
        {
            wrapperSymbols: ['WireResponse'],
            machineryIndicators: ['headers', 'status', 'ok'],
            originModuleGlobs: [HTTP_PACKAGE, `${HTTP_PACKAGE}/*`],
            unwrapRecursively: false,
        },
    ],
};
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
const WIDGET = '{ id: string; label: string; }';
describe('carrick#2714: a consumer reads the body, never the transport response object', () => {
    let client;
    let repoDir;
    let servicePath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2714-'));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        const pkgDir = path.join(repoDir, 'node_modules', HTTP_PACKAGE);
        fs.mkdirSync(pkgDir, { recursive: true });
        fs.writeFileSync(path.join(pkgDir, 'package.json'), JSON.stringify({ name: HTTP_PACKAGE, version: '1.0.0', types: 'index.d.ts' }));
        fs.writeFileSync(path.join(pkgDir, 'index.d.ts'), HTTP_PACKAGE_DTS);
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                lib: ['es2022', 'dom'],
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        servicePath = path.join(repoDir, 'src', 'service.ts');
        fs.writeFileSync(servicePath, SERVICE_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    /** The locator a scan sends: the call's text on its line. */
    async function infer(alias, expressionText) {
        const lines = SERVICE_TS.split('\n');
        const index = lines.findIndex((line) => line.includes(expressionText));
        assert.ok(index >= 0, `fixture must contain ${expressionText}`);
        const res = await client.send({
            action: 'infer',
            request_id: alias,
            requests: [
                {
                    file_path: servicePath,
                    line_number: index + 1,
                    infer_kind: 'call_result',
                    alias,
                    expression_text: expressionText,
                    expression_line: index + 1,
                },
            ],
            extraction_config: EXTRACTION_CONFIG,
        });
        const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
        assert.ok(inferred, `expected an answer for ${alias}`);
        return inferred;
    }
    function assertStatedBody(inferred, expected) {
        assert.strictEqual(collapse(inferred.type_string), expected, JSON.stringify(inferred));
        assert.strictEqual(inferred.is_explicit, true, JSON.stringify(inferred));
        assert.notStrictEqual(inferred.primary_type_symbol, 'HttpResponse');
    }
    function assertDecidedTransport(inferred) {
        assert.strictEqual(inferred.type_string.trim(), 'unknown', JSON.stringify(inferred));
        assert.ok((inferred.any_provenance ?? []).some((p) => p.path === '' && p.reason === 'machinery_envelope'), `the abstain must be decided, so the capture does not re-read the call: ${JSON.stringify(inferred)}`);
        assert.strictEqual(inferred.primary_type_symbol, undefined);
    }
    it('a cast on the payload member is the body, reported as stated', async () => {
        const inferred = await infer('Cast', 'http.get("/v1/widgets/cast")');
        assertStatedBody(inferred, WIDGET);
        assert.strictEqual(inferred.stated_body?.root, 'Widget');
        assert.strictEqual(inferred.primary_type_symbol, 'Widget');
    });
    it('an annotation on the name the payload member initialises is the body', async () => {
        assertStatedBody(await infer('Annotated', 'http.get("/v1/widgets/annotated")'), WIDGET);
    });
    it('the declared return type of the function that returns the payload member is the body', async () => {
        assertStatedBody(await infer('Declared', 'http.get("/v1/widgets/declared")'), WIDGET);
    });
    it('an untyped payload member the source states nothing about is a decided abstain', async () => {
        assertDecidedTransport(await infer('Unstated', 'http.get("/v1/widgets/unstated")'));
    });
    it('a response object returned where the function declares Promise<void> is a decided abstain, not void', async () => {
        assertDecidedTransport(await infer('Void', 'http.post("/v1/widgets/ping", {})'));
    });
    it('a response object handed on whole is a decided abstain', async () => {
        assertDecidedTransport(await infer('Whole', 'http.delete("/v1/widgets/removed")'));
    });
    it('a result written through a conditional alias: the cast on the payload member is the body', async () => {
        assertStatedBody(await infer('ResultCast', 'results.get("/v1/widgets/result-cast")'), WIDGET);
    });
    it('a result written through a conditional alias, returned where the function declares Promise<void>, is a decided abstain', async () => {
        assertDecidedTransport(await infer('ResultVoid', 'results.post("/v1/widgets/result-ping", {})'));
    });
    it('a client that returns the body by default is not read as the response object another branch names', async () => {
        const inferred = await infer('BodyClient', 'bodies.fetch("/v1/widgets/body")');
        assert.strictEqual(inferred.type_string, 'Widget', JSON.stringify(inferred));
        assert.deepStrictEqual((inferred.any_provenance ?? []).filter((p) => p.path === ''), []);
    });
    it('a response with no payload member is left to its body reads, as before', async () => {
        // `WireResponse` is the package's too, and the `HttpResponse` rule's
        // members and origin match it, but it has no `data` member: its body is
        // read (`res.json()`), so this site keeps the answer it had and is not
        // decided as a response object around a member.
        const inferred = await infer('Sent', 'send("/v1/widgets/sent")');
        assert.deepStrictEqual((inferred.any_provenance ?? []).filter((p) => p.path === ''), [], JSON.stringify(inferred));
    });
    it('a payload member read in place on the awaited call is the body', async () => {
        assertStatedBody(await infer('InPlace', 'http.get("/v1/widgets/in-place")'), WIDGET);
    });
    it('a payload member destructured out of the response object is the body', async () => {
        assertStatedBody(await infer('Destructured', 'http.get("/v1/widgets/destructured")'), WIDGET);
    });
    it('two different statements for the payload member decide nothing', async () => {
        assertDecidedTransport(await infer('Two', 'http.get("/v1/widgets/two")'));
    });
    it('a type parameter fixed by an argument is not free, so its default is not the result', async () => {
        const inferred = await infer('Echo', 'echo(widget)');
        assert.deepStrictEqual((inferred.any_provenance ?? []).filter((p) => p.path === ''), [], JSON.stringify(inferred));
        assert.strictEqual(inferred.type_string, 'Widget', JSON.stringify(inferred));
    });
    it('a closed statement at the body read is kept over a later recast', async () => {
        // Only a placeholder is narrowed: a closed statement keeps the answer it
        // had, its anchor included (the call's: the platform `Response`).
        const inferred = await infer('Restated', 'apiFetch("/v1/widgets/restated")');
        assert.strictEqual(collapse(inferred.type_string), `{ data: ${WIDGET}[]; }`, JSON.stringify(inferred));
        assert.strictEqual(inferred.primary_type_symbol, 'Response', JSON.stringify(inferred));
    });
    it('a call that states its payload type keeps that payload', async () => {
        const inferred = await infer('Typed', 'http.get<Widget>("/v1/widgets/typed")');
        assert.strictEqual(inferred.type_string, 'Widget', JSON.stringify(inferred));
        assert.strictEqual(inferred.primary_type_symbol, 'Widget');
    });
    it('a placeholder cast at the body read narrowed later states the envelope, not its element', async () => {
        const inferred = await infer('Narrowed', 'apiFetch("/v1/widgets")');
        assert.strictEqual(collapse(inferred.type_string), `{ data: ${WIDGET}[]; }`, JSON.stringify(inferred));
        assert.strictEqual(inferred.is_explicit, true);
        assert.ok(inferred.stated_body, `the narrowed statement is the stated body: ${JSON.stringify(inferred)}`);
    });
});
/**
 * Blueprint row N33: a zero-argument `json()` or `text()` is the platform's
 * body read only where the runtime declares the value it is called on: a
 * global the compiler's library or an installed runtime typings package
 * declares (`fetch`'s `Response`). The member's name picks the read; the
 * declaration decides whether it is one. A repository's own class that
 * happens to have a `json()` or `text()` member reads its own value.
 */
const OWN_READS_TS = `export interface Widget {
  id: string;
  label: string;
}

export class Envelope {
  constructor(private readonly raw: string) {}
  json(): { size: number } {
    return { size: this.raw.length };
  }
  text(): string {
    return this.raw;
  }
}

export declare function load(url: string): Promise<Envelope>;

export async function ownJson() {
  const env = await load("/v1/own-json");
  return env.json();
}

export async function ownText() {
  const env = await load("/v1/own-text");
  return env.text();
}

export async function platformJson() {
  const res = await fetch("/v1/platform-json");
  return (await res.json()) as Widget;
}

export async function platformText() {
  const res = await fetch("/v1/platform-text");
  return res.text();
}
`;
/** A runtime typings package: the platform's `Response` declared as a global from a module. */
const RUNTIME_TYPES_DTS = `export {};
declare global {
  interface Response {
    readonly ok: boolean;
    readonly status: number;
    json(): Promise<unknown>;
    text(): Promise<string>;
  }
  function fetch(input: string): Promise<Response>;
}
`;
// 'nothing declared': `fetch` resolves to nothing (no DOM library, no runtime
// typings installed), so its value is `any` and the name alone decides, as before.
for (const runtime of ['default library', 'runtime typings', 'nothing declared']) {
    describe(`blueprint row N33: body reads are the platform's by declaration (${runtime})`, () => {
        let client;
        let repoDir;
        let servicePath;
        before(async () => {
            repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-n33-'));
            fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
            if (runtime === 'runtime typings') {
                const typesDir = path.join(repoDir, 'node_modules', '@types', 'node');
                fs.mkdirSync(typesDir, { recursive: true });
                fs.writeFileSync(path.join(typesDir, 'package.json'), JSON.stringify({ name: '@types/node', version: '22.0.0', types: 'index.d.ts' }));
                fs.writeFileSync(path.join(typesDir, 'index.d.ts'), RUNTIME_TYPES_DTS);
            }
            fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
                compilerOptions: {
                    strict: true,
                    module: 'esnext',
                    moduleResolution: 'bundler',
                    target: 'es2022',
                    lib: runtime === 'default library' ? ['es2022', 'dom'] : ['es2022'],
                    types: runtime === 'nothing declared' ? [] : undefined,
                    skipLibCheck: true,
                },
                include: ['src'],
            }));
            servicePath = path.join(repoDir, 'src', 'service.ts');
            fs.writeFileSync(servicePath, OWN_READS_TS);
            client = new SidecarClient();
            await client.start();
            await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
        });
        after(async () => {
            await client.stop();
            fs.rmSync(repoDir, { recursive: true, force: true });
        });
        async function infer(alias, expressionText) {
            const lines = OWN_READS_TS.split('\n');
            const index = lines.findIndex((line) => line.includes(expressionText));
            assert.ok(index >= 0, `fixture must contain ${expressionText}`);
            const res = await client.send({
                action: 'infer',
                request_id: alias,
                requests: [
                    {
                        file_path: servicePath,
                        line_number: index + 1,
                        infer_kind: 'call_result',
                        alias,
                        expression_text: expressionText,
                        expression_line: index + 1,
                    },
                ],
            });
            const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
            assert.ok(inferred, `expected an answer for ${alias}`);
            return inferred;
        }
        it("fetch's json() is the body read, and the cast on it is the body", async () => {
            const inferred = await infer('PlatformJson', 'fetch("/v1/platform-json")');
            assert.strictEqual(collapse(inferred.type_string), WIDGET, JSON.stringify(inferred));
        });
        it("fetch's text() is a raw-text body read", async () => {
            const inferred = await infer('PlatformText', 'fetch("/v1/platform-text")');
            // Nothing declared: the read is taken, but it yields `any`, not a string.
            const expected = runtime === 'nothing declared' ? undefined : true;
            assert.strictEqual(inferred.raw_text_read, expected, JSON.stringify(inferred));
        });
        it("a repository class's own text() is not a raw-text body read", async () => {
            const inferred = await infer('OwnText', 'load("/v1/own-text")');
            assert.strictEqual(inferred.raw_text_read, undefined, JSON.stringify(inferred));
        });
        it("a repository class's own json() is not the parsed body: the call answers its own value", async () => {
            const inferred = await infer('OwnJson', 'load("/v1/own-json")');
            assert.strictEqual(collapse(inferred.type_string), 'Envelope', JSON.stringify(inferred));
        });
    });
}
