/**
 * carrick#1842: a consumer that reads the body as raw text states no
 * structural contract, so the inferrer says so beside the `string` it
 * publishes, and the check phase reads that pair unverifiable, as it does a
 * body of bytes (carrick#1812).
 *
 * The published `string` stays: it is what the call site reads. What changes
 * is that the row now carries `raw_text_read`, because `string` alone cannot
 * tell a text read from a JSON body that is a string.
 *
 * Two shapes of a text read, and nothing else:
 *  - a zero-argument `.text()` read of the call's own response, the platform
 *    `Response` here, on its binding or in a `then` callback;
 *  - a call whose resolved signature, at this site, types one member of an
 *    object argument as exactly the literal `'text'` the source passes, which
 *    is how a request library lets a caller choose a text body
 *    (`type: 'text'`), through a wrapper rule or without one.
 *
 * A json read whose type is `string`, a call that returns `string` with no
 * text choice, and a `'text'` member a signature types as a wider union are
 * all left unmarked: none of them is a raw-text read.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

/** A request library that lets the caller pick the body format. */
const LIBRARY_DTS = `export type BodyFormat = "text" | "json" | "blob";
type FormatMap = { text: string; json: unknown; blob: Blob };
export interface Reply<T> {
  status: number;
  ok: boolean;
  body: T;
  url: string;
  headers: Headers;
}
export interface RequestConfig<F extends BodyFormat> {
  url: string;
  method?: "GET" | "POST";
  format: F;
}
export declare function request<F extends BodyFormat>(
  config: RequestConfig<F>
): Promise<Reply<FormatMap[F]>>;
export declare function fetchBody<F extends BodyFormat>(
  url: string,
  options: { format: F }
): Promise<FormatMap[F]>;
export declare function getLabel(url: string): Promise<string>;
export declare function sendMessage(message: {
  kind: "text" | "image";
  body: string;
}): Promise<string>;
`;

const SERVICE_TS = `import { request, fetchBody, getLabel, sendMessage } from "body-lib";

export async function readText(): Promise<string> {
  const res = await fetch("/reads/text-binding");
  if (!res.ok) {
    throw new Error("failed");
  }
  const receipt = await res.text();
  return receipt;
}

export async function returnText(): Promise<string> {
  const res = await fetch("/reads/text-return");
  return res.text();
}

export function thenText(): Promise<string> {
  return fetch("/reads/text-then").then((res) => res.text());
}

interface Receipt {
  id: string;
}

export async function parsedText(): Promise<Receipt> {
  const res = await fetch("/reads/parsed-text");
  const receipt: Receipt = JSON.parse(await res.text());
  return receipt;
}

export async function jsonString(): Promise<string> {
  const res = await fetch("/reads/json-string");
  const label = (await res.json()) as string;
  return label;
}

export async function libraryText() {
  const reply = await request({ url: "/reads/library-text", method: "POST", format: "text" });
  return reply;
}

export async function libraryJson() {
  const reply = await request({ url: "/reads/library-json", format: "json" });
  return reply;
}

export async function bodyText(): Promise<string> {
  const body = await fetchBody("/reads/body-text", { format: "text" });
  return body;
}

export async function plainString(): Promise<string> {
  const label = await getLabel("/reads/plain-string");
  return label;
}

export async function messageText(): Promise<string> {
  const id = await sendMessage({ kind: "text", body: "/reads/message-text" });
  return id;
}
`;

/** The service's wrapper rule for the library's response object. */
const REPLY_RULE = {
  rules: [
    {
      wrapperSymbols: ['Reply'],
      originModuleGlobs: ['body-lib', 'body-lib/*'],
      payloadGenericIndex: 0,
      unwrapRecursively: true,
    },
  ],
};

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    raw_text_read?: boolean;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

/** The 1-based line of the one source line containing `marker`. */
function lineOf(marker: string): number {
  const lines = SERVICE_TS.split('\n');
  const hits = lines.flatMap((line, index) => (line.includes(marker) ? [index + 1] : []));
  assert.strictEqual(hits.length, 1, `marker ${marker} must name one line`);
  return hits[0];
}

describe('carrick#1842: a raw-text read is marked beside the string it publishes', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1842-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const libDir = path.join(repoDir, 'node_modules', 'body-lib');
    fs.mkdirSync(libDir, { recursive: true });
    fs.writeFileSync(
      path.join(libDir, 'package.json'),
      JSON.stringify({ name: 'body-lib', version: '1.0.0', types: './index.d.ts' })
    );
    fs.writeFileSync(path.join(libDir, 'index.d.ts'), LIBRARY_DTS);
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
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

  /** The locator a scan really sends: the model's expression text and its line. */
  async function infer(alias: string, expressionText: string, withRule = false) {
    const line = lineOf(expressionText);
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      ...(withRule ? { extraction_config: REPLY_RULE } : {}),
      requests: [
        {
          file_path: servicePath,
          line_number: line,
          infer_kind: 'call_result',
          alias,
          expression_text: expressionText,
          expression_line: line,
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  for (const [name, expression] of [
    ['a text read through a binding', 'fetch("/reads/text-binding")'],
    ['a returned text read', 'fetch("/reads/text-return")'],
    ['a text read in a then callback', 'fetch("/reads/text-then")'],
  ] as const) {
    it(`marks ${name}`, async () => {
      const inferred = await infer(`Endpoint_${name.replace(/\W+/g, '_')}_Response`, expression);
      assert.ok(inferred, 'the row must be answered');
      assert.strictEqual(collapse(inferred.type_string), 'string');
      assert.strictEqual(inferred.raw_text_read, true);
    });
  }

  it('marks a library call whose signature takes the text format, unwrapped by a rule', async () => {
    const inferred = await infer(
      'Endpoint_LibraryText_Response',
      'request({ url: "/reads/library-text", method: "POST", format: "text" })',
      true
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'string');
    assert.strictEqual(inferred.raw_text_read, true);
  });

  it('marks a library call that answers the text body directly', async () => {
    const inferred = await infer(
      'Endpoint_BodyText_Response',
      'fetchBody("/reads/body-text", { format: "text" })'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'string');
    assert.strictEqual(inferred.raw_text_read, true);
  });

  it('does not mark a text read the source parses into a type it states', async () => {
    const inferred = await infer('Endpoint_ParsedText_Response', 'fetch("/reads/parsed-text")');
    assert.ok(inferred, 'the row must be answered');
    assert.notStrictEqual(collapse(inferred.type_string), 'string');
    assert.strictEqual(inferred.raw_text_read, undefined);
  });

  it('does not mark a json read whose type is string', async () => {
    const inferred = await infer('Endpoint_JsonString_Response', 'fetch("/reads/json-string")');
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'string');
    assert.strictEqual(inferred.raw_text_read, undefined);
  });

  it('does not mark the json format of the same library call', async () => {
    const inferred = await infer(
      'Endpoint_LibraryJson_Response',
      'request({ url: "/reads/library-json", format: "json" })',
      true
    );
    // The json format yields no `string` to mark; abstaining is also fine.
    assert.strictEqual(inferred?.raw_text_read, undefined);
  });

  it('does not mark a call that returns string with no text choice', async () => {
    const inferred = await infer('Endpoint_PlainString_Response', 'getLabel("/reads/plain-string")');
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'string');
    assert.strictEqual(inferred.raw_text_read, undefined);
  });

  it('does not mark a text member the signature types as a wider union', async () => {
    const inferred = await infer(
      'Endpoint_MessageText_Response',
      'sendMessage({ kind: "text", body: "/reads/message-text" })'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'string');
    assert.strictEqual(inferred.raw_text_read, undefined);
  });
});
