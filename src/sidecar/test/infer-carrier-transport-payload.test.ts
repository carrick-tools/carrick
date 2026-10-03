/**
 * Regression for carrick#1841: a consumer call whose result carrier holds a
 * request library's own response object published that object as the
 * response contract.
 *
 * The client below returns `Task<Outcome<Reply<string>, WireError>>`. The
 * service's wrapper rules name all three wrappers, but a rule never matched:
 * `Task` and `Reply` are type aliases (of a class and of an object literal),
 * and the matcher reads the type's own symbol first. So the #1376 carrier
 * read peeled the thenable and the outcome by shape and published what the
 * success side holds, `Reply<string>`: the library's status, url and headers
 * around the body, not the body. Judged against a producer's body, every one
 * of those members read as one the producer does not send.
 *
 * The carrier's payload is now offered to the same rules before it is
 * published. A rule that verifies it as the library's transport object and
 * reads no payload out of it makes the site decide it states no contract
 * (`machinery_envelope` at the root, no anchor). A rule that does read a
 * payload out of it publishes that payload, printed with its own members.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const PACKAGE_DTS = `export declare class __Task<A> {
  then(func: (value: A) => void): this;
}
export type Task<A> = __Task<A>;

declare class __Outcome<A, E> {
  isDone(): boolean;
}
interface Done<A, E> extends __Outcome<A, E> {
  readonly tag: "Done";
  readonly value: A;
}
interface Failed<A, E> extends __Outcome<A, E> {
  readonly tag: "Failed";
  readonly error: E;
}
export type Outcome<A, E> = Done<A, E> | Failed<A, E>;

export type Maybe<A> = { present: true; value: A } | { present: false };

type BodyKinds = { text: string; json: unknown };

export type Reply<T> = {
  status: number;
  ok: boolean;
  response: Maybe<T>;
  url: string;
  headers: Record<string, string>;
};

export interface Envelope<T> {
  data: T;
  requestId: string;
}

export declare class WireError extends Error {
  url: string;
}

export declare const Wire: {
  make: <K extends keyof BodyKinds>(config: {
    url: string;
    method?: "GET" | "POST";
    type: K;
  }) => Task<Outcome<Reply<BodyKinds[K]>, WireError>>;
  enveloped: <T>(url: string) => Task<Outcome<Envelope<T>, WireError>>;
};
`;

const SERVICE_TS = `import { Wire } from "tiny-wire";

export interface Invoice {
  id: string;
  total: number;
}

export function ping(): void {
  Wire.make({ url: "/v1/ping", method: "POST", type: "text" });
}

export function sendInvite(id: string) {
  return Wire.make({ url: \`/v1/invitations/\${id}/send\`, method: "POST", type: "json" });
}

export function loadInvoice(id: string) {
  return Wire.enveloped<Invoice>(\`/v1/invoices/\${id}\`);
}
`;

/** 1-based lines in SERVICE_TS, read off the source above. */
const PING_LINE = 9;
const INVITE_LINE = 13;
const INVOICE_LINE = 17;

/** The service's wrapper rules, shaped like the ones a scan receives. */
const EXTRACTION_CONFIG = {
  rules: [
    {
      wrapperSymbols: ['Reply'],
      machineryIndicators: ['headers', 'status', 'statusText', 'ok', 'body'],
      originModuleGlobs: ['tiny-wire', 'tiny-wire/*'],
      payloadGenericIndex: 0,
      unwrapRecursively: true,
      maxDepth: 3,
    },
    {
      wrapperSymbols: ['Task'],
      originModuleGlobs: ['tiny-wire', 'tiny-wire/*'],
      payloadGenericIndex: 0,
      unwrapRecursively: true,
      maxDepth: 4,
    },
    {
      wrapperSymbols: ['Outcome'],
      originModuleGlobs: ['tiny-wire', 'tiny-wire/*'],
      payloadGenericIndex: 0,
      unwrapRecursively: true,
      maxDepth: 4,
    },
    {
      wrapperSymbols: ['Envelope'],
      originModuleGlobs: ['tiny-wire', 'tiny-wire/*'],
      payloadGenericIndex: 0,
      unwrapRecursively: true,
      maxDepth: 3,
    },
  ],
};

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    primary_type_symbol?: string;
    array_depth?: number;
    any_provenance?: Array<{ path: string; kind: string; reason: string; detail?: string }>;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe("carrick#1841: a carrier's payload passes the service's wrapper rules", () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1841-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const pkgDir = path.join(repoDir, 'node_modules', 'tiny-wire');
    fs.mkdirSync(pkgDir, { recursive: true });
    fs.writeFileSync(
      path.join(pkgDir, 'package.json'),
      JSON.stringify({ name: 'tiny-wire', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(pkgDir, 'index.d.ts'), PACKAGE_DTS);
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022'],
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

  /** The locator a scan really sends, with or without the service's rules. */
  async function infer(
    alias: string,
    line: number,
    expressionText: string,
    withRules = true
  ) {
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
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
      ...(withRules ? { extraction_config: EXTRACTION_CONFIG } : {}),
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  function assertDecidedTransport(
    inferred: NonNullable<Awaited<ReturnType<typeof infer>>>
  ): void {
    assert.strictEqual(
      collapse(inferred.type_string),
      'unknown',
      `the library's response object is not the body, got: ${inferred.type_string}`
    );
    const root = (inferred.any_provenance ?? []).filter((p) => p.path === '');
    assert.deepStrictEqual(
      root.map((p) => [p.kind, p.reason]),
      [['unknown', 'machinery_envelope']],
      'the abstain must say it was decided, or the capture re-reads the raw call'
    );
    assert.strictEqual(
      inferred.primary_type_symbol,
      undefined,
      'no anchor may name the transport object, or the surface declares it'
    );
  }

  it('abstains on a text read whose carrier holds the library response object', async () => {
    const inferred = await infer(
      'Endpoint_Ping_Response',
      PING_LINE,
      'Wire.make({ url: "/v1/ping", method: "POST", type: "text" })'
    );
    assert.ok(inferred, 'the row must be answered');
    assertDecidedTransport(inferred);
  });

  it('abstains the same way on a json read the caller returns unread', async () => {
    const inferred = await infer(
      'Endpoint_Invite_Response',
      INVITE_LINE,
      'Wire.make({ url: `/v1/invitations/${id}/send`, method: "POST", type: "json" })'
    );
    assert.ok(inferred, 'the row must be answered');
    assertDecidedTransport(inferred);
  });

  it("publishes the payload a rule reads out of the carrier's payload", async () => {
    // `Task<Outcome<Envelope<Invoice>, WireError>>`: the carrier holds the
    // library's envelope, and the service's rule for that envelope names its
    // payload argument. That argument is the body, printed with its own
    // members (#257), and it anchors the row.
    const inferred = await infer(
      'Endpoint_Invoice_Response',
      INVOICE_LINE,
      'Wire.enveloped<Invoice>(`/v1/invoices/${id}`)'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), '{ id: string; total: number; }');
    assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
  });

  it('keeps the carrier payload where the service has no rule for it', async () => {
    // Without a rule nothing here knows the object is the library's own, and
    // telling it apart by its member names is the shape heuristic this fix
    // does not add. The row keeps today's answer.
    const inferred = await infer(
      'Endpoint_PingNoRules_Response',
      PING_LINE,
      'Wire.make({ url: "/v1/ping", method: "POST", type: "text" })',
      false
    );
    assert.ok(inferred, 'the row must be answered');
    assert.match(inferred.type_string, /headers:/);
  });
});
