/**
 * carrick#2200: a handler that returns its response as data, an object with a
 * numeric `statusCode` (or `status`) and a `body` string it serialised itself,
 * publishes the value it serialised, never the envelope.
 *
 * The envelope is transport: `{ statusCode: number; body: string }` says
 * nothing about what a caller receives. The contract is the argument of the
 * `JSON.stringify` that produced `body`, read from each returned expression:
 * an inline literal, or one hop into a builder the repo declares in its own
 * source. The status member decides success or error, as a status argument
 * does on a framework send, so error branches drop out (carrick#1161).
 *
 * A returned branch these rules cannot read makes the whole answer `unknown`
 * with the decided reason `serialised_body_unread`. Publishing the branches
 * that did read would state part of the contract as the whole of it, and
 * publishing the envelope or `string` would be a wrong contract.
 *
 * An object-typed `body` is not an envelope under this rule: a runtime that
 * serialises the handler's return value sends that object as the payload, so
 * the shape is ambiguous and keeps today's answer.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

/**
 * A response builder an installed package ships as source. Its body reads
 * exactly as the repo's own builder does, so only the origin gate keeps it
 * out: what an installed package does is not this repository's contract.
 */
const RESPONDER_TS = `export interface PackagedEnvelope {
  statusCode: number;
  body: string;
}
export function respond(code: number, payload: unknown): PackagedEnvelope {
  return { statusCode: code, body: JSON.stringify(payload) };
}
`;

const NOTES_TS = `import { respond } from "responder-runtime";

export interface Note {
  id: string;
  title: string;
}

export interface Envelope {
  statusCode: number;
  headers?: Record<string, string>;
  body: string;
}

export interface AddNoteResult {
  id: string;
  title: string;
}

export interface CreditResult {
  balance: number;
}

/** A platform result type whose members are all optional. */
export interface PlatformResult {
  statusCode?: number;
  headers?: Record<string, string>;
  body?: string;
}

declare const store: { list(): Note[]; add(title: string): Note };
declare const flag: boolean;
declare function toCsv(rows: Note[]): string;
declare function credit(): Promise<CreditResult>;

function reply(code: number, payload: unknown): Envelope {
  try {
    const body = JSON.stringify(payload);
    return { statusCode: code, headers: { "content-type": "application/json" }, body };
  } catch {
    return { statusCode: 500, body: JSON.stringify({ error: "serialisation failed" }) };
  }
}

function send<T>(statusCode: number, payload: T): Envelope {
  return { statusCode, body: JSON.stringify(payload) };
}

const ok = (payload: unknown): Envelope => ({ statusCode: 200, body: JSON.stringify(payload) });

function notFound(payload: unknown): Envelope {
  return { statusCode: 404, body: JSON.stringify(payload) };
}

function platform(code: number, payload: unknown): PlatformResult {
  return { statusCode: code, body: JSON.stringify(payload) };
}

function log(ctx: unknown, code: number, label: string, out: Envelope): Envelope {
  void ctx;
  void code;
  void label;
  return out;
}

export async function listNotes(): Promise<Envelope> { // LIST
  const notes = store.list();
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ notes }),
  };
}

export async function createNote(title: string): Promise<Envelope> { // CREATE
  if (!title) return reply(400, { error: "title required" });
  const note = store.add(title);
  return reply(201, { id: note.id, created: true });
}

export async function addNote(title: string) { // ADD
  if (!title) return send(400, { error: "title required" });
  return send<AddNoteResult>(201, { id: "n1", title });
}

export async function addNoteInferred(title: string) { // ADD_INFERRED
  return send(201, { id: "n1", title });
}

export async function findNote(id: string) { // FIND
  const note = store.list().find((n) => n.id === id);
  return note ? ok({ note }) : notFound({ error: "no such note" });
}

export async function platformNote(): Promise<PlatformResult> { // PLATFORM
  return platform(200, { count: store.list().length });
}

declare const unavailable: Envelope;
declare function settle(): Promise<Note | undefined>;

function respondWith(code: number, payload: CreditResult): Envelope {
  return { statusCode: code, body: JSON.stringify(payload) };
}

export async function passThrough(ctx: unknown) { // PASS_THROUGH
  if (flag) return log(ctx, 503, "unavailable", unavailable);
  return log(ctx, 200, "ok", reply(200, { up: true }));
}

export async function settled() { // CALLBACKS
  return settle().then(
    (note) => (note ? ok({ note }) : notFound({ error: "no such note" })),
    () => notFound({ error: "lookup failed" })
  );
}

export async function declaredPayload() { // DECLARED
  return respondWith(200, { balance: 1 });
}

function wide(code: number, payload: Record<string, unknown>): Envelope {
  return { statusCode: code, body: JSON.stringify(payload) };
}

export async function widePayload() { // WIDE
  return wide(200, { total: 3 });
}

export async function archive() { // ARCHIVE
  if (flag) return reply(200, { archived: true });
  const out: Envelope = { statusCode: 200, body: JSON.stringify({ archived: false }) };
  return out;
}

export async function exportCsv(): Promise<Envelope> { // CSV
  return { statusCode: 200, body: toCsv(store.list()) };
}

export async function packaged() { // PACKAGED
  return respond(200, { packaged: true });
}

export async function unknownPayload(raw: unknown): Promise<Envelope> { // UNKNOWN_PAYLOAD
  return { statusCode: 200, body: JSON.stringify(raw) };
}

export async function objectBody() { // OBJECT_BODY
  const id = "n1";
  return { status: 200, body: { id } };
}

export async function creditNow(): Promise<CreditResult> { // NOT_ENVELOPE
  return credit();
}
`;

/** A rule that names the envelope and reads its `body` member as the payload. */
const ENVELOPE_RULE_CONFIG = {
  rules: [
    {
      wrapperSymbols: ['Envelope'],
      payloadPropertyPath: ['body'],
      unwrapRecursively: false,
    },
  ],
};

function lineOf(marker: string): number {
  const idx = NOTES_TS.split('\n').findIndex((l) => l.includes(`// ${marker}`));
  assert.ok(idx >= 0, `fixture must contain: // ${marker}`);
  return idx + 1;
}

interface Provenance {
  path: string;
  kind: string;
  reason: string;
  detail?: string;
}

interface Inferred {
  alias: string;
  type_string: string;
  is_explicit: boolean;
  primary_type_symbol?: string;
  any_provenance?: Provenance[];
}

interface InferShape {
  status: string;
  inferred_types?: Inferred[];
  errors?: string[];
}

function collapse(text: string): string {
  return text.replace(/\s+/g, ' ').trim();
}

describe('carrick#2200: a serialised-body envelope publishes what it serialised', () => {
  let repoDir: string;
  let client: SidecarClient;
  let notesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2200-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const pkgDir = path.join(repoDir, 'node_modules', 'responder-runtime');
    fs.mkdirSync(pkgDir, { recursive: true });
    fs.writeFileSync(
      path.join(pkgDir, 'package.json'),
      JSON.stringify({ name: 'responder-runtime', version: '1.0.0', types: './index.ts' })
    );
    fs.writeFileSync(path.join(pkgDir, 'index.ts'), RESPONDER_TS);
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          rootDir: 'src',
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    notesPath = path.join(repoDir, 'src', 'notes.ts');
    fs.writeFileSync(notesPath, NOTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function inferReturn(
    marker: string,
    extractionConfig?: unknown
  ): Promise<Inferred> {
    const alias = `Endpoint_${marker}_Response`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: notesPath,
          line_number: lineOf(marker),
          infer_kind: 'function_return',
          alias,
        },
      ],
      ...(extractionConfig ? { extraction_config: extractionConfig } : {}),
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, `${marker} must answer, got ${JSON.stringify(res)}`);
    return inferred;
  }

  function assertUnread(inferred: Inferred): void {
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
    assert.strictEqual(inferred.is_explicit, false);
    const provenance = inferred.any_provenance ?? [];
    assert.strictEqual(provenance.length, 1, JSON.stringify(provenance));
    assert.strictEqual(provenance[0].path, '');
    assert.strictEqual(provenance[0].kind, 'unknown');
    assert.strictEqual(provenance[0].reason, 'serialised_body_unread');
    assert.ok(
      !/\//.test(provenance[0].detail ?? ''),
      `the reason must carry no path, got: ${provenance[0].detail}`
    );
  }

  function assertNoEnvelope(inferred: Inferred): void {
    const text = collapse(inferred.type_string);
    assert.ok(!/statusCode/.test(text), `the envelope leaked: ${text}`);
    assert.ok(!/body: string/.test(text), `the envelope leaked: ${text}`);
    assert.notStrictEqual(text, 'string', 'the serialised string is not the contract');
  }

  it('reads an inline JSON.stringify in a returned envelope literal', async () => {
    const inferred = await inferReturn('LIST');
    assertNoEnvelope(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ notes: { id: string; title: string; }[]; }'
    );
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('reads one hop into a builder, through a const, and drops its error branches', async () => {
    const inferred = await inferReturn('CREATE');
    assertNoEnvelope(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ id: string; created: boolean; }',
      'the reply(400, ...) branch is an error and the builder catch is its failure path'
    );
    assert.strictEqual(inferred.is_explicit, false);
  });

  it('publishes a type argument the builder is called with, as explicit', async () => {
    const inferred = await inferReturn('ADD');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ id: string; title: string; }');
    assert.strictEqual(inferred.is_explicit, true);
    assert.strictEqual(inferred.primary_type_symbol, 'AddNoteResult');
  });

  it('keeps an inferred type argument implicit', async () => {
    const inferred = await inferReturn('ADD_INFERRED');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ id: string; title: string; }');
    assert.strictEqual(inferred.is_explicit, false);
    assert.strictEqual(inferred.primary_type_symbol, undefined);
  });

  it('reads a builder whose literal status decides the branch', async () => {
    const inferred = await inferReturn('FIND');
    assertNoEnvelope(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ note: { id: string; title: string; }; }',
      'the notFound(...) builder states only a 404, so a call to it is an error branch'
    );
  });

  it('reads an envelope type whose status and body are optional', async () => {
    const inferred = await inferReturn('PLATFORM');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ count: number; }');
  });

  it('drops a pass-through stating an error status, and reads one stating success', async () => {
    const inferred = await inferReturn('PASS_THROUGH');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ up: boolean; }');
  });

  it('reads the callbacks a returned call hands the envelope to', async () => {
    const inferred = await inferReturn('CALLBACKS');
    assertNoEnvelope(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ note: { id: string; title: string; }; }'
    );
  });

  it('publishes the payload type a builder declares, as explicit', async () => {
    const inferred = await inferReturn('DECLARED');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ balance: number; }');
    assert.strictEqual(inferred.is_explicit, true);
    assert.strictEqual(inferred.primary_type_symbol, 'CreditResult');
  });

  it('reads the argument when the builder declares a type that names no member', async () => {
    const inferred = await inferReturn('WIDE');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ total: number; }');
    assert.strictEqual(inferred.is_explicit, false);
  });

  it('answers unknown when one branch returns an identifier', async () => {
    assertUnread(await inferReturn('ARCHIVE'));
  });

  it('answers unknown when the body is not produced by JSON.stringify', async () => {
    assertUnread(await inferReturn('CSV'));
  });

  it('answers unknown when the builder is an installed package', async () => {
    assertUnread(await inferReturn('PACKAGED'));
  });

  it('answers unknown when the serialised value is typed unknown', async () => {
    assertUnread(await inferReturn('UNKNOWN_PAYLOAD'));
  });

  it('never publishes string for a rule that reads the envelope body', async () => {
    const inferred = await inferReturn('LIST', ENVELOPE_RULE_CONFIG);
    assertNoEnvelope(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ notes: { id: string; title: string; }[]; }'
    );
  });

  it('keeps today\'s answer for an object-typed body', async () => {
    const inferred = await inferReturn('OBJECT_BODY');
    assert.strictEqual(
      inferred.type_string,
      PINNED_OBJECT_BODY,
      'an object body is ambiguous with a runtime that serialises the return'
    );
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('leaves a return that is not an envelope unchanged', async () => {
    const inferred = await inferReturn('NOT_ENVELOPE');
    assert.strictEqual(inferred.type_string, PINNED_NOT_ENVELOPE);
    assert.strictEqual(inferred.is_explicit, true);
    assert.strictEqual(inferred.any_provenance, undefined);
  });
});

/** Pinned from origin/main before carrick#2200, byte for byte. */
const PINNED_OBJECT_BODY = '{ status: number; body: { id: string; }; }';
const PINNED_NOT_ENVELOPE = '{ balance: number; }';
