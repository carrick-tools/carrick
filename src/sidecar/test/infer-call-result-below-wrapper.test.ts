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

const CALLERS_TS = `import { relay, timed, forward, viaClient, readsItself, callApi, Order } from './wrappers';

export const orders = () => callApi<Order[]>('/orders');

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
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-below-wrapper', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (alias: string, belowCall: string, decidingCall: string, result?: { whole: true }) => {
    const at = spanOf(WRAPPERS_TS, belowCall);
    const through = spanOf(CALLERS_TS, decidingCall);
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: wrappersPath,
          line_number: lineAt(WRAPPERS_TS, at.start),
          span_start: at.start,
          span_end: at.end,
          infer_kind: 'call_result',
          alias,
          ...(result ? { result } : {}),
          through_call: { file_path: callersPath, span_start: through.start, span_end: through.end },
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
});
