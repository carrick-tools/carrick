/**
 * Regression for carrick#1841: a consumer call whose result carrier holds a
 * request library's own response object published that object as the
 * response contract.
 *
 * The client below returns `Task<Outcome<Reply<T>, WireError>>`, and the
 * service has rules for what a result holds (`Reply`, `Envelope`) and none
 * for the thenable or the outcome around it. The carrick#1376 carrier read
 * peels those two by shape and finds what the success side holds,
 * `Reply<T>`: the library's status, url and headers around the body, not the
 * body. Judged against a producer's body, every one of those members read as
 * one the producer does not send.
 *
 * The carrier's payload is offered to the service's rules before it is
 * published. A rule that verifies it as the library's transport object and
 * reads no payload out of it makes the site decide it states no contract
 * (`machinery_envelope` at the root, no anchor). A rule that does read a
 * payload out of it publishes that payload: a shape printed with its own
 * members, or the `string` of a body read as text, marked as raw text
 * (carrick#1842).
 *
 * `Reply` is a type alias, so its rule read no payload out of it until
 * carrick#1843, and the text read below abstained with the json one. A
 * service whose rules name all three wrappers never reaches the carrier
 * read: test/infer-alias-wrapper-rules.test.ts.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import { SidecarClient } from './helpers.js';
import {
  WIRE_ORIGIN,
  collapse,
  writeWireRepo,
  type WireInferred,
} from './wire-package.js';

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

/**
 * The service's rules for what a result holds, shaped like the ones a scan
 * receives. None names the thenable or the outcome, so the carrier read is
 * what reaches the payload.
 */
const EXTRACTION_CONFIG = {
  rules: [
    {
      wrapperSymbols: ['Reply'],
      machineryIndicators: ['headers', 'status', 'statusText', 'ok', 'body'],
      originModuleGlobs: WIRE_ORIGIN,
      payloadGenericIndex: 0,
      unwrapRecursively: true,
      maxDepth: 3,
    },
    {
      wrapperSymbols: ['Envelope'],
      originModuleGlobs: WIRE_ORIGIN,
      payloadGenericIndex: 0,
      unwrapRecursively: true,
      maxDepth: 3,
    },
  ],
};

interface InferShape {
  inferred_types?: WireInferred[];
}

describe("carrick#1841: a carrier's payload passes the service's wrapper rules", () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    const repo = writeWireRepo('carrick-1841-', { 'service.ts': SERVICE_TS });
    repoDir = repo.repoDir;
    servicePath = repo.pathOf('service.ts');

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

  it('publishes a text read as the string the rule reads out, marked as raw text', async () => {
    // `Reply<string>`: the rule reads the alias's argument (carrick#1843), and
    // the body read as text states no structural contract (carrick#1842).
    // The mark is what keeps `string` from being judged against a JSON body.
    const inferred = await infer(
      'Endpoint_Ping_Response',
      PING_LINE,
      'Wire.make({ url: "/v1/ping", method: "POST", type: "text" })'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'string');
    assert.strictEqual(inferred.raw_text_read, true);
    assert.strictEqual(inferred.primary_type_symbol, undefined);
  });

  it('abstains on a json read whose carrier holds the library response object', async () => {
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
