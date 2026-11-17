/**
 * A call result read below a wrapper (`through_call` names the call that
 * reaches the wrapper) that finds only transport falls back to the read at the
 * deciding call, where the caller states what it reads: a cast, an annotation
 * or a text read. A read below the wrapper that finds a body keeps it.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const WRAPPERS_TS = `import { http } from 'tiny-http';
declare function pull<K extends 'json' | 'text'>(url: string, config: { type: K }): Promise<K extends 'text' ? string : unknown>;

// Hands the transport on: the caller reads the body off it.
export function relay(route: string) {
  return fetch(\`/relay\${route}\`);
}

// Holds the transport in an object beside a value of its own.
export async function timed(route: string) {
  const started = Date.now();
  const res = await fetch(\`/timed\${route}\`);
  return { res, took: Date.now() - started };
}

// Forwards the caller's own format choice to the transport.
export function forward<K extends 'json' | 'text'>(route: string, options: { type: K }) {
  return pull(\`/forward\${route}\`, options);
}

// A request library's response object around the body, handed on.
export function viaClient(route: string) {
  return http.get(\`/client\${route}\`);
}

// Forwards the caller's type argument to the client (carrick#4513).
export interface Order { id: string }
export async function callApi<T>(route: string): Promise<T> {
  const res = await http.get<T>(\`/api\${route}\`);
  return res.data;
}

// Reads the body itself: the read below the call finds a body.
export async function readsItself(route: string) {
  const res = await fetch(\`/own\${route}\`);
  const body = (await res.json()) as { own: number };
  return body;
}
`;

// carrick#4513 box shape: one generic helper and its callers in the SAME
// file. The helper reads the JSON into its own type parameter and hands it
// back as a member of an object of its own; each caller passes an inline
// type argument and reads `body` off the result.
const SAME_FILE_TS = `import { http } from 'tiny-http';

export async function call<T>(path: string, init?: RequestInit): Promise<{ status: number; body: T }> {
  const res = await fetch(\`/call\${path}\`, init);
  const body: T = await res.json();
  return { status: res.status, body };
}

export async function callCast<T>(path: string): Promise<{ status: number; body: T }> {
  const res = await fetch(\`/cast\${path}\`);
  const body = (await res.json()) as T;
  return { status: res.status, body };
}

export async function readToken(path: string): Promise<string | null> {
  const res = await fetch(\`/token\${path}\`);
  if (!res.ok) return null;
  return res.text();
}

export async function readNote(path: string): Promise<string | null> {
  const res = await fetch(\`/note\${path}\`);
  const text = await res.text();
  return text;
}

export async function callData<T>(path: string): Promise<{ status: number; body: T }> {
  const res = await http.get(\`/data\${path}\`);
  const body: T = res.data;
  return { status: res.status, body };
}

export async function callDataCast<T>(path: string): Promise<{ status: number; body: T }> {
  const res = await http.get(\`/datacast\${path}\`);
  const body = res.data as T;
  return { status: res.status, body };
}

export async function readFlag(path: string): Promise<string | null> {
  const res = await fetch(\`/flag\${path}\`);
  return res.ok ? await res.text() : null;
}

export async function loadOrder() {
  const { body } = await call<{ id: number; slug: string }>('/orders/1');
  return body;
}

export async function loadCount() {
  return (await callCast<{ count: number }>('/counts')).body;
}

export const data = () => callData<{ id: number }>('/one');
export const dataCast = () => callDataCast<{ id: number }>('/one');
export const flag = () => readFlag('/me');
export const token = () => readToken('/me');
export const note = () => readNote('/me');
`;

const CALLERS_TS = `import { relay, timed, forward, viaClient, readsItself, callApi, Order } from './wrappers';

export const orders = () => callApi<Order[]>('/orders');
export const skus = () => callApi<{ sku: string }>('/skus');

export async function viaRelay() {
  const res = await relay('/item');
  return (await res.json()) as { id: number };
}

export async function viaTimed() {
  const { res } = await timed('/session');
  return (await res.json()) as { token: string };
}

export async function viaRelayText() {
  const res = await relay('/ping');
  return await res.text();
}

export async function viaForward() {
  return await forward('/logout', { type: 'text' });
}

export async function viaForwardJson() {
  return await forward('/other', { type: 'json' });
}

export async function viaClientCall() {
  const res = await viaClient('/widget');
  return res.data as { serial: number };
}

export async function viaSelf() {
  return await readsItself('/own');
}
`;

const HTTP_PACKAGE = 'tiny-http';
const HTTP_PACKAGE_DTS = `export interface HttpResponse<T = any> {
  data: T;
  status: number;
  statusText: string;
  headers: Record<string, string>;
}
export interface HttpClient {
  get<T = any>(url: string): Promise<HttpResponse<T>>;
}
export declare const http: HttpClient;
`;

/** The rule a scan receives for the package's response object. */
const EXTRACTION_CONFIG = {
  rules: [
    {
      wrapperSymbols: ['HttpResponse'],
      machineryIndicators: ['status', 'statusText', 'headers'],
      originModuleGlobs: [HTTP_PACKAGE, `${HTTP_PACKAGE}/*`],
      payloadGenericIndex: 0,
      payloadPropertyPath: ['data'],
      unwrapRecursively: true,
      maxDepth: 4,
    },
  ],
};

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    raw_text_read?: boolean;
    any_provenance?: Array<{ reason: string }>;
    primary_type_symbol?: string;
    type_home?: { symbol?: string };
  }>;
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('a call result read below a wrapper falls back to the deciding call', () => {
  let client: SidecarClient;
  let repoDir: string;
  let wrappersPath: string;
  let callersPath: string;
  let sameFilePath: string;

  const spanOf = (source: string, text: string): { start: number; end: number } => {
    const start = source.indexOf(text);
    assert.ok(start >= 0, `fixture holds no ${JSON.stringify(text)}`);
    return { start, end: start + text.length };
  };
  const lineAt = (source: string, offset: number): number => source.slice(0, offset).split('\n').length;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-below-wrapper-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['es2022', 'dom'], skipLibCheck: true },
        include: ['src'],
      })
    );
    const pkgDir = path.join(repoDir, 'node_modules', HTTP_PACKAGE);
    fs.mkdirSync(pkgDir, { recursive: true });
    fs.writeFileSync(path.join(pkgDir, 'package.json'), JSON.stringify({ name: HTTP_PACKAGE, version: '1.0.0', types: 'index.d.ts' }));
    fs.writeFileSync(path.join(pkgDir, 'index.d.ts'), HTTP_PACKAGE_DTS);
    wrappersPath = path.join(repoDir, 'src', 'wrappers.ts');
    callersPath = path.join(repoDir, 'src', 'callers.ts');
    fs.writeFileSync(wrappersPath, WRAPPERS_TS);
    fs.writeFileSync(callersPath, CALLERS_TS);
    sameFilePath = path.join(repoDir, 'src', 'same-file.ts');
    fs.writeFileSync(sameFilePath, SAME_FILE_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-below-wrapper', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (
    alias: string,
    belowCall: string,
    decidingCall: string,
    result?: { whole: true } | { member: string[] },
    files: { below: [string, string]; deciding: [string, string] } = {
      below: [wrappersPath, WRAPPERS_TS],
      deciding: [callersPath, CALLERS_TS],
    }
  ) => {
    const [belowPath, belowSource] = files.below;
    const [decidingPath, decidingSource] = files.deciding;
    const at = spanOf(belowSource, belowCall);
    const through = spanOf(decidingSource, decidingCall);
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: belowPath,
          line_number: lineAt(belowSource, at.start),
          span_start: at.start,
          span_end: at.end,
          infer_kind: 'call_result',
          alias,
          ...(result ? { result } : {}),
          through_call: { file_path: decidingPath, span_start: through.start, span_end: through.end },
        },
      ],
      extraction_config: EXTRACTION_CONFIG,
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(inferred, `no answer for ${alias}: ${JSON.stringify(response.errors)}`);
    return {
      text: collapse(inferred.type_string),
      provenance: (inferred.any_provenance ?? []).map((p) => p.reason),
      rawText: inferred.raw_text_read === true,
      anchor: inferred.primary_type_symbol,
      home: inferred.type_home?.symbol,
    };
  };

  it('a wrapper that hands the transport on reads the cast at the caller', async () => {
    const { text, provenance } = await infer('Relay', 'fetch(`/relay${route}`)', "relay('/item')");
    assert.match(text, /^\{ id: number;? \}$/, text);
    assert.deepStrictEqual(provenance, []);
  });

  it('a wrapper that holds the transport in an object reads the cast at the caller', async () => {
    const { text, provenance } = await infer('Timed', 'fetch(`/timed${route}`)', "timed('/session')");
    assert.match(text, /^\{ token: string;? \}$/, text);
    assert.deepStrictEqual(provenance, []);
  });

  it('a raw text read below a wrapper stays a raw text read', async () => {
    const { text, rawText } = await infer('RelayText', 'fetch(`/relay${route}`)', "relay('/ping')");
    assert.strictEqual(text, 'string');
    assert.strictEqual(rawText, true);
  });

  it('a text format forwarded through a wrapper stays a raw text read', async () => {
    const { text, rawText } = await infer(
      'Forward',
      'pull(`/forward${route}`, options)',
      "forward('/logout', { type: 'text' })",
      { whole: true }
    );
    assert.strictEqual(text, 'string');
    assert.strictEqual(rawText, true);
  });

  it('a text format that is not chosen leaves a read unmarked', async () => {
    const { rawText } = await infer('ForwardJson', 'pull(`/forward${route}`, options)', "forward('/other', { type: 'json' })", {
      whole: true,
    });
    assert.strictEqual(rawText, false);
  });

  it('a response object handed on reads the body the caller states', async () => {
    const { text, provenance } = await infer('Client', 'http.get(`/client${route}`)', "viaClient('/widget')");
    assert.match(text, /^\{ serial: number;? \}$/, text);
    assert.deepStrictEqual(provenance, []);
  });

  it('a read below the wrapper that finds a body keeps it', async () => {
    const { text, provenance } = await infer('Self', 'fetch(`/own${route}`)', "readsItself('/own')");
    assert.match(text, /^\{ own: number;? \}$/, text);
    assert.deepStrictEqual(provenance, []);
  });

  it('a generic wrapper anchors on the type argument its caller passes, never its own parameter', async () => {
    const { text, anchor, home } = await infer('Generic', 'http.get<T>(`/api${route}`)', "callApi<Order[]>('/orders')");
    assert.strictEqual(text, 'Order[]');
    assert.strictEqual(anchor, 'Order');
    assert.notStrictEqual(home, 'T');
  });

  it('a generic wrapper given an inline type argument anchors nothing, never its own parameter', async () => {
    const { text, anchor } = await infer('Inline', 'http.get<T>(`/api${route}`)', "callApi<{ sku: string }>('/skus')");
    assert.match(text, /^\{ sku: string;? \}$/, text);
    assert.strictEqual(anchor, undefined);
  });

  // carrick#4513 box shape. The read is made where the request is (the
  // wrapper hands back an object of its own), and the caller holds the
  // value at the same spot of the call's result, with its type argument.
  const sameFile = () => ({
    below: [sameFilePath, SAME_FILE_TS] as [string, string],
    deciding: [sameFilePath, SAME_FILE_TS] as [string, string],
  });

  it('a JSON read annotated with the wrapper T reads the member the caller holds, never T', async () => {
    const { text, anchor } = await infer(
      'SameFileAnnotated',
      'fetch(`/call${path}`, init)',
      "call<{ id: number; slug: string }>('/orders/1')",
      undefined,
      sameFile()
    );
    assert.match(text, /^\{ id: number; slug: string;? \}$/, text);
    assert.notStrictEqual(anchor, 'T');
  });

  it('a JSON read cast to the wrapper T reads the member the caller holds, never T', async () => {
    const { text, anchor } = await infer(
      'SameFileCast',
      'fetch(`/cast${path}`)',
      "callCast<{ count: number }>('/counts')",
      undefined,
      sameFile()
    );
    assert.match(text, /^\{ count: number;? \}$/, text);
    assert.notStrictEqual(anchor, 'T');
  });

  it('a JSON read stated as the wrapper T, with no call to bind it, states no type', async () => {
    for (const [alias, below] of [
      ['OwnAnnotated', 'fetch(`/call${path}`, init)'],
      ['OwnCast', 'fetch(`/cast${path}`)'],
    ] as const) {
      const at = spanOf(SAME_FILE_TS, below);
      const response = await client.send<InferShape>({
        action: 'infer',
        request_id: `infer-${alias}`,
        requests: [
          {
            file_path: sameFilePath,
            line_number: lineAt(SAME_FILE_TS, at.start),
            span_start: at.start,
            span_end: at.end,
            infer_kind: 'call_result',
            alias,
          },
        ],
        extraction_config: EXTRACTION_CONFIG,
      });
      const inferred = response.inferred_types?.find((t) => t.alias === alias);
      assert.ok(inferred, `no answer for ${alias}: ${JSON.stringify(response.errors)}`);
      assert.strictEqual(inferred.type_string, 'unknown', alias);
      assert.deepStrictEqual((inferred.any_provenance ?? []).map((p) => p.reason), ['bound_by_callers'], alias);
    }
  });

  it('a labelled read stated as the wrapper T never states T, labelled or not, bound or not', async () => {
    const results = [undefined, { whole: true as const }, { member: ['data'] }];
    for (const [name, below, deciding] of [
      ['Data', 'http.get(`/data${path}`)', "callData<{ id: number }>('/one')"],
      ['DataCast', 'http.get(`/datacast${path}`)', "callDataCast<{ id: number }>('/one')"],
    ] as const) {
      for (const [index, result] of results.entries()) {
        const at = spanOf(SAME_FILE_TS, below);
        const through = spanOf(SAME_FILE_TS, deciding);
        for (const bound of [false, true]) {
          const alias = `${name}${index}${bound ? 'Bound' : 'Own'}`;
          const response = await client.send<InferShape>({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
              {
                file_path: sameFilePath,
                line_number: lineAt(SAME_FILE_TS, at.start),
                span_start: at.start,
                span_end: at.end,
                infer_kind: 'call_result',
                alias,
                ...(result ? { result } : {}),
                ...(bound ? { through_call: { file_path: sameFilePath, span_start: through.start, span_end: through.end } } : {}),
              },
            ],
            extraction_config: EXTRACTION_CONFIG,
          });
          const inferred = response.inferred_types?.find((t) => t.alias === alias);
          if (!inferred) continue;
          const text = collapse(inferred.type_string).replace(/import\("[^"]*"\)/g, '');
          assert.doesNotMatch(text, /\bT\b/, `${alias}: ${inferred.type_string}`);
          assert.notStrictEqual(inferred.primary_type_symbol, 'T', alias);
        }
      }
    }
  });

  it('a text read behind a conditional null keeps the null the caller holds', async () => {
    const { text, rawText } = await infer('SameFileConditionalNull', 'fetch(`/flag${path}`)', "readFlag('/me')", undefined, sameFile());
    assert.strictEqual(text, 'string | null');
    assert.strictEqual(rawText, false);
  });

  it('a text read behind an early null keeps the null the caller holds', async () => {
    const { text } = await infer('SameFileEarlyNull', 'fetch(`/token${path}`)', "readToken('/me')", undefined, sameFile());
    assert.strictEqual(text, 'string | null');
  });

  it('a text read under a declared nullable return keeps the null the caller holds', async () => {
    const { text } = await infer('SameFileDeclaredNull', 'fetch(`/note${path}`)', "readNote('/me')", undefined, sameFile());
    assert.strictEqual(text, 'string | null');
  });
});
