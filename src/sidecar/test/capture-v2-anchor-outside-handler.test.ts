/**
 * Stale-position T2, the capture half: a node the text or line locator found
 * is a route row's own only inside the handler or a function the handler's
 * return reaches (`anchorInHandler`, capture/site-node.ts).
 *
 * The box abstains this rule was corrected for are all capture `infer`
 * anchors with no span, so each shape is driven through `captureStub` with
 * the `handler_lines` the scanner stamps:
 *
 * - a request read, or a reply-chain send, inside a helper the handler
 *   returns is the row's own (bound by the reach set, not the handler's
 *   lines);
 * - a text found outside the reach set is looked for inside it, and read
 *   there when it names one answer; never at the stale node;
 * - a helper the handler calls but does not return, a handler that reads no
 *   body, a text that names two answers inside the handler and a `.map`
 *   callback's return stay unknown.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

const FRAMEWORK_TS = `export interface Incoming {
  body: unknown;
  json<T>(): Promise<T>;
}
export interface Reply {
  send(payload: unknown): void;
  status(code: number): Reply;
  text(words: string): string;
  page(words: string): Response;
}
export type Handler = (request: Incoming, reply: Reply) => unknown;
export interface Server {
  get(path: string, handler: Handler): void;
  post(path: string, handler: Handler): void;
  patch(path: string, handler: Handler): void;
}
export interface DraftBody { draftTitle: string; pages: number }
export interface InvoiceRow { invoiceRef: string; amountDue: number }
export interface SummaryBody { openCount: number }
export interface DigestBody { digestLines: string[] }
export interface OrderBody { sku: string; quantity: number }
export interface TallyBody { tallied: number }
`;

const ROUTES_TS = `import type {
  DigestBody,
  DraftBody,
  Incoming,
  InvoiceRow,
  OrderBody,
  Reply,
  Server,
  SummaryBody,
  TallyBody,
} from './framework.js';

const saveDraft = async (request: Incoming, reply: Reply) => {
  const draft = await request.json<DraftBody>();
  return { stored: draft.draftTitle };
};

function issueInvoice(request: Incoming, reply: Reply) {
  const invoice: InvoiceRow = { invoiceRef: 'i1', amountDue: 10 };
  return reply.status(201).send(invoice);
}

function sayPlain(reply: Reply, words: string) {
  return reply.text(words);
}

function sayPage(reply: Reply, words: string) {
  return reply.page(words);
}

export function routes(server: Server, ids: string[]): void {
  server.post('/drafts', (request, reply) => saveDraft(request, reply));

  server.post('/bills', (request, reply) => issueInvoice(request, reply));

  server.patch('/bills', (request, reply) => issueInvoice(request, reply));

  server.get('/summary', async (request, reply) => {
    const response: SummaryBody = { openCount: 1 };
    response.openCount += 1;
    return response;
  });

  server.get('/digest', async (request, reply) => {
    const response: DigestBody = { digestLines: [] };
    return response;
  });

  server.get('/ping', (request, reply) => { sayPlain(reply, 'ping'); });

  server.get('/pong', (request, reply) => { sayPlain(reply, 'pong'); });

  server.post('/orders', async (request, reply) => {
    const payload = request.body as OrderBody;
    reply.send({ total: payload.quantity });
  });

  server.post('/touch', (request, reply) => { reply.send({ touched: true }); });

  server.post('/flag', (request, reply) => { reply.send({ payload: true }); });

  server.get('/hello', (request, reply) => sayPage(reply, 'hello'));

  server.get('/bye', (request, reply) => sayPage(reply, 'bye'));

  server.get('/left', async (request, reply) => {
    const row: TallyBody = { tallied: 1 };
    reply.send(row);
  });

  server.get('/right', async (request, reply) => {
    if (ids.length > 0) {
      const row: TallyBody = { tallied: 2 };
      reply.send(row);
      return;
    }
    const row: DigestBody = { digestLines: [] };
    reply.send(row);
  });

  server.get('/slots', async (request, reply) => {
    const labels = ids.map((id) => ({ id, label: 'slot' }));
    return { count: labels.length, labels };
  });
}
`;

const LINES = ROUTES_TS.split('\n');
function lineWith(fragment: string, from = 1): number {
  const idx = LINES.findIndex((l, i) => i + 1 >= from && l.includes(fragment));
  assert.ok(idx >= 0, `fixture must contain ${fragment}`);
  return idx + 1;
}

/** The handler lines the scanner stamps: one line, or through the next `});`. */
function handler(opening: string, oneLine = false): { start_line: number; end_line: number } {
  const start = lineWith(opening);
  return { start_line: start, end_line: oneLine ? start : lineWith('  });', start) };
}

interface Ask {
  kind: string;
  text: string;
  line: number;
  handler_lines: { start_line: number; end_line: number };
}

const ASKS: Record<string, Ask> = {
  // (1) the request is read in the helper the one-line arrow returns.
  draftRequest: {
    kind: 'request_body',
    text: 'request.json<DraftBody>()',
    line: lineWith('const draft = await request.json<DraftBody>()'),
    handler_lines: handler("server.post('/drafts'", true),
  },
  // (2) two routes return one helper whose return is a reply-chain send.
  billPost: {
    kind: 'response_body',
    text: 'invoice',
    line: lineWith('return reply.status(201).send(invoice)'),
    handler_lines: handler("server.post('/bills'", true),
  },
  billPatch: {
    kind: 'response_body',
    text: 'invoice',
    line: lineWith('return reply.status(201).send(invoice)'),
    handler_lines: handler("server.patch('/bills'", true),
  },
  // (3) the first `response` from the hint line on is the next route's.
  summaryReturn: {
    kind: 'function_return',
    text: 'response',
    line: lineWith('return response') + 1,
    handler_lines: handler("server.get('/summary'"),
  },
  // Must abstain: the helper is called, never returned.
  pingText: {
    kind: 'response_body',
    text: 'reply.text(words)',
    line: lineWith('return reply.text(words)'),
    handler_lines: handler("server.get('/ping'", true),
  },
  pongText: {
    kind: 'response_body',
    text: 'reply.text(words)',
    line: lineWith('return reply.text(words)'),
    handler_lines: handler("server.get('/pong'", true),
  },
  // Must abstain: the handler reads no body.
  touchRequest: {
    kind: 'request_body',
    text: 'payload',
    line: lineWith('const payload = request.body as OrderBody'),
    handler_lines: handler("server.post('/touch'", true),
  },
  // Must abstain: the hint is spelled in the handler only as an object key.
  flagRequest: {
    kind: 'request_body',
    text: 'payload',
    line: lineWith('const payload = request.body as OrderBody'),
    handler_lines: handler("server.post('/flag'", true),
  },
  // Must abstain: the returned helper answers with a Response, no body.
  helloPage: {
    kind: 'response_body',
    text: 'reply.page(words)',
    line: lineWith('return reply.page(words)'),
    handler_lines: handler("server.get('/hello'", true),
  },
  byePage: {
    kind: 'response_body',
    text: 'reply.page(words)',
    line: lineWith('return reply.page(words)'),
    handler_lines: handler("server.get('/bye'", true),
  },
  // Must abstain: inside the handler the text reads two types.
  rightSend: {
    kind: 'response_body',
    text: 'row',
    line: lineWith('reply.send(row)'),
    handler_lines: handler("server.get('/right'"),
  },
  // Must abstain: a `.map` callback's return is not the route's.
  slotsMap: {
    kind: 'function_return',
    text: "{ id, label: 'slot' }",
    line: lineWith('ids.map((id)'),
    handler_lines: handler("server.get('/slots'"),
  },
};

describe('stale-position T2: a capture anchor is bound by the handler and what its return reaches', () => {
  let repoDir: string;
  let outRoot: string;
  let result: CaptureStubResult;
  let surface: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-t2-capture-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-t2-capture-stub-'));
    fs.mkdirSync(path.join(repoDir, 'src'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'framework.ts'), FRAMEWORK_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'routes.ts'), ROUTES_TS);

    result = captureStub({
      repoRoot: repoDir,
      serviceName: 'anchor-bounds-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: Object.entries(ASKS).map(([name, ask]) => ({
        kind: 'infer' as const,
        alias: `Capture_${name}`,
        source_file: 'src/routes.ts',
        anchor_origin: 'deterministic-infer' as const,
        expression_text: ask.text,
        line_number: ask.line,
        infer_kind: ask.kind,
        handler_lines: ask.handler_lines,
      })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  function captured(name: string): { text: string; reason: string | undefined } {
    const alias = `Capture_${name}`;
    const record = result.aliases.find((a) => a.alias === alias);
    assert.ok(record, `no capture record for ${alias}`);
    const m = surface.match(
      new RegExp(`export type ${alias} = ([\\s\\S]*?);(?=\\s*(?:export type |$))`)
    );
    assert.ok(m, `no surface line for ${alias}:\n${surface}`);
    return {
      text: m![1].replace(/\s+/g, ' ').trim(),
      reason: record!.self_check_detail,
    };
  }

  function assertRead(name: string, wanted: RegExp): void {
    const got = captured(name);
    assert.match(got.text, wanted, `${name}: ${JSON.stringify(got)}`);
  }

  function assertUnknown(name: string): void {
    const got = captured(name);
    assert.strictEqual(got.text, 'unknown', `${name}: ${JSON.stringify(got)}`);
  }

  it('(1) a request read in the helper a one-line arrow returns is the route request', () => {
    assertRead('draftRequest', /DraftBody|draftTitle/);
  });

  it('(2) a send in the helper two routes return is each route response', () => {
    assertRead('billPost', /InvoiceRow|amountDue/);
    assertRead('billPatch', /InvoiceRow|amountDue/);
  });

  it("(3) a return text found in the next route is read in the route's own handler", () => {
    const got = captured('summaryReturn');
    assert.doesNotMatch(got.text, /DigestBody|digestLines/, JSON.stringify(got));
    assert.match(got.text, /SummaryBody|openCount/, JSON.stringify(got));
  });

  it('a text reply in a helper the handler calls but does not return stays unknown', () => {
    assertUnknown('pingText');
    assertUnknown('pongText');
  });

  it('a request hint for a handler that reads no body stays unknown', () => {
    assertUnknown('touchRequest');
  });

  it('a request hint spelled only as an object key in the handler stays unknown', () => {
    assertUnknown('flagRequest');
  });

  it('a Response a returned helper answers with stays unknown', () => {
    assertUnknown('helloPage');
    assertUnknown('byePage');
  });

  it('a text that reads two types inside the handler stays unknown', () => {
    assertUnknown('rightSend');
  });

  it("a .map callback's return stays unknown", () => {
    assertUnknown('slotsMap');
  });
});
