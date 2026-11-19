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
/** A platform result type whose members are all optional. */
export interface PlatformResult {
  statusCode?: number;
  headers?: Record<string, string>;
  body?: string;
}
export function respond(code: number, payload: unknown): PackagedEnvelope {
  return { statusCode: code, body: JSON.stringify(payload) };
}
/** A scope guard an installed package ships: its error branch states a 403. */
export async function guardScope(
  id: string
): Promise<{ scope: string } | { error: PackagedEnvelope }> {
  if (!id) return { error: { statusCode: 403, body: JSON.stringify({ error: "denied" }) } };
  return { scope: id };
}
`;

const NOTES_TS = `import { guardScope, respond, type PlatformResult } from "responder-runtime";

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
  if (flag) return reply(200, { rows: store.list().length });
  return { statusCode: 200, body: toCsv(store.list()) };
}

export async function exportCsvOnly(): Promise<Envelope> { // CSV_ONLY
  return { statusCode: 200, body: toCsv(store.list()) };
}

export async function packaged() { // PACKAGED
  if (flag) return reply(200, { local: true });
  return respond(200, { packaged: true });
}

export interface Article {
  id: string;
  title: string;
  body: string;
  status: number;
}

declare const article: Article;

export async function getArticle() { // ARTICLE
  return article;
}

export async function greeting() { // GREETING
  return { status: 1, body: "hello" };
}

declare const prebuilt: PlatformResult;

declare const articleStore: { update(id: string): Promise<Partial<Article>> };

export async function patchArticle(id: string): Promise<Partial<Article>> { // PARTIAL
  return articleStore.update(id);
}

declare const saver: { create(a: Omit<Article, "id">): Promise<Article> };

export async function storeJson(doc: { tags: string[] }): Promise<Article> { // STORE_JSON
  return saver.create({ title: "t", body: JSON.stringify(doc), status: 1 });
}

declare function withFallback(run: () => Envelope, fallback: Envelope): Envelope;

export async function withFallbackReply() { // FALLBACK
  return withFallback(() => reply(200, { a: 1 }), reply(200, { b: "x" }));
}

export async function platformAsIs(): Promise<PlatformResult> { // PLATFORM_AS_IS
  return prebuilt;
}

declare function forward(ev: unknown, edit: (r: Envelope) => void): Envelope;

export async function forwarded(ev: unknown) { // FORWARD
  if (flag) return forward(ev, (r) => { r.headers = { "x": "1" }; });
  return reply(200, { id: "1" });
}

declare function firstOf(a: Envelope, b: Envelope): Envelope;

export async function either() { // TWO_ENVELOPES
  return firstOf(reply(200, { a: 1 }), reply(200, { b: "x" }));
}

declare const loose: { [x: string]: any };
declare const wrapped: { data: any };

export async function indexAny() { // INDEX_ANY
  return reply(200, loose);
}

export async function memberAny() { // MEMBER_ANY
  return reply(200, { wrapped });
}

export async function sendAny() { // SEND_ANY
  return send<any>(200, { x: 1 });
}

export async function sendUnknown() { // SEND_UNKNOWN
  return send<unknown>(200, { x: 1 });
}

type Json = string | number | boolean | null | Json[] | { [key: string]: Json };

function objectReply(code: number, payload: object): Envelope {
  return { statusCode: code, body: JSON.stringify(payload) };
}

function jsonReply(code: number, payload: Json): Envelope {
  return { statusCode: code, body: JSON.stringify(payload) };
}

export async function objectParam() { // OBJECT_PARAM
  return objectReply(200, { k: 1 });
}

export async function jsonParam() { // JSON_PARAM
  return jsonReply(200, { j: "x" });
}

function withTimeout(ctx: unknown, code: number, out: Envelope): Envelope {
  void ctx;
  void code;
  return out;
}

export async function timed(ctx: unknown) { // TIMEOUT
  return withTimeout(ctx, 500, reply(200, { late: false }));
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

// carrick#2205: a returned envelope built earlier, one def-use hop away.

export async function constBuilder(id: string) { // CONST_BUILDER
  if (!id) return reply(400, { error: "id required" });
  const out = reply(201, { id });
  return out;
}

export async function letBuilder(id: string) { // LET_BUILDER
  if (!id) return reply(200, { id: "none" });
  let out = reply(201, { id });
  return out;
}

export async function chainedConsts(id: string) { // CHAINED
  if (!id) return reply(200, { id: "none" });
  const first = reply(201, { id });
  const second = first;
  return second;
}

export async function rewritten(id: string) { // REWRITTEN
  if (!id) return reply(200, { id: "none" });
  const out = reply(201, { id });
  out.body = JSON.stringify({ replaced: true });
  return out;
}

export async function deleted(id: string) { // DELETED
  if (!id) return reply(200, { id: "none" });
  const out = reply(201, { id });
  delete out.headers;
  return out;
}

declare function save(input: Envelope): Envelope;

export async function savedInput(doc: { tags: string[] }) { // SAVED_INPUT
  if (flag) return reply(200, { saved: false });
  const out = save({ statusCode: 200, body: JSON.stringify(doc) });
  return out;
}

export async function callbackConst(ids: string[]) { // CALLBACK_CONST
  if (flag) return reply(200, { count: 0 });
  return ids.map((id) => {
    const out = reply(200, { id });
    return out;
  })[0];
}

const deny = () => ({ error: notFound({ error: "missing" }) });

async function resolveScope(id: string): Promise<{ scope: string } | { error: Envelope }> {
  if (!id) return { error: reply(403, { error: "denied" }) };
  if (id === "gone") return deny();
  if (id === "late") return { error: log(null, 503, "late", reply(503, { error: "later" })) };
  return { scope: id };
}

export async function scoped(id: string) { // MEMBER_ERROR
  const resolved = await resolveScope(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

async function scopeWithCode(id: string, code: number): Promise<{ scope: string } | { error: Envelope }> {
  if (!id) return { error: reply(code, { error: "denied" }) };
  return { scope: id };
}

export async function scopedCode(id: string) { // MEMBER_VARIABLE
  const resolved = await scopeWithCode(id, 403);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

async function scopeWithSuccess(id: string): Promise<{ scope: string } | { error: Envelope }> {
  if (!id) return { error: reply(200, { scope: "default" }) };
  return { scope: id };
}

export async function scopedSuccess(id: string) { // MEMBER_SUCCESS
  const resolved = await scopeWithSuccess(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

async function scopeForwarded(id: string): Promise<{ scope: string } | { error: Envelope }> {
  const inner = await resolveScope(id);
  if ("error" in inner) return { error: inner.error };
  return { scope: inner.scope };
}

export async function scopedForwarded(id: string) { // MEMBER_SECOND_HOP
  const resolved = await scopeForwarded(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

const failure = { error: reply(403, { error: "denied" }) };

async function scopeSpread(id: string): Promise<{ scope: string } | { error: Envelope }> {
  if (!id) return { error: reply(403, { error: "denied" }) };
  if (flag) return { ...failure };
  return { scope: id };
}

export async function scopedSpread(id: string) { // MEMBER_SPREAD
  const resolved = await scopeSpread(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

async function scopeShorthand(id: string): Promise<{ scope: string } | { error: Envelope }> {
  const error = reply(200, { scope: "default" });
  if (!id) return { error: reply(403, { error: "denied" }) };
  if (flag) return { error };
  return { scope: id };
}

export async function scopedShorthand(id: string) { // MEMBER_SHORTHAND
  const resolved = await scopeShorthand(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

declare const cachedFailure: { error: Envelope };

async function scopeOpaque(id: string): Promise<{ scope: string } | { error: Envelope }> {
  if (!id) return { error: reply(403, { error: "denied" }) };
  if (flag) return cachedFailure;
  return { scope: id };
}

export async function scopedOpaque(id: string) { // MEMBER_OPAQUE
  const resolved = await scopeOpaque(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

function scopeLoop(id: string): { scope: string } | { error: Envelope } {
  if (!id) return { error: reply(403, { error: "denied" }) };
  if (flag) return scopeLoop(id.slice(1));
  return { scope: id };
}

export async function scopedLoop(id: string) { // MEMBER_RECURSIVE
  const resolved = scopeLoop(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

async function scopeWritten(id: string): Promise<{ scope: string } | { error: Envelope }> {
  if (!id) return { error: reply(403, { error: "denied" }) };
  return { scope: id };
}

export async function scopedWritten(id: string) { // MEMBER_WRITTEN
  const resolved = await scopeWritten(id);
  if ("error" in resolved) {
    resolved.error.statusCode = 200;
    return resolved.error;
  }
  return reply(200, { scope: resolved.scope });
}

async function scopeNeverFails(id: string): Promise<{ scope: string } | { error: Envelope }> {
  return { scope: id };
}

export async function scopedAbsent(id: string) { // MEMBER_ABSENT
  const resolved = await scopeNeverFails(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

export async function scopedInstalled(id: string) { // MEMBER_INSTALLED
  const resolved = await guardScope(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

async function scopeSaved(id: string): Promise<{ scope: string } | { error: Envelope }> {
  if (!id) return { error: save({ statusCode: 403, body: JSON.stringify({ error: "denied" }) }) };
  return { scope: id };
}

export async function scopedSaved(id: string) { // MEMBER_CALL_INPUT
  const resolved = await scopeSaved(id);
  if ("error" in resolved) return resolved.error;
  return reply(200, { scope: resolved.scope });
}

export async function articleViaConst() { // ARTICLE_CONST
  const found = article;
  return found;
}

export async function articleBuiltViaConst(doc: { tags: string[] }) { // ARTICLE_BUILT_CONST
  const built = { id: "a1", title: "t", body: JSON.stringify(doc), status: 200 };
  return built;
}

export async function articleBesideReply() { // ARTICLE_BESIDE_REPLY
  if (flag) return reply(200, { ok: true });
  const found = article;
  return found;
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

  function assertUnread(inferred: Inferred, detail: RegExp): void {
    assert.strictEqual(collapse(inferred.type_string), 'unknown', inferred.alias);
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
    assert.match(provenance[0].detail ?? '', detail);
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

  it('reads a returned const initialised by an envelope literal', async () => {
    const inferred = await inferReturn('ARCHIVE');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ archived: boolean; }');
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('reads a returned const initialised by a builder call', async () => {
    const inferred = await inferReturn('CONST_BUILDER');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ id: string; }');
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('answers unknown for a returned let, a second hop, or a const in another function', async () => {
    for (const marker of ['LET_BUILDER', 'CHAINED', 'CALLBACK_CONST']) {
      assertUnread(await inferReturn(marker), /built somewhere else/);
    }
  });

  it('answers unknown for a returned const written after it was built', async () => {
    for (const marker of ['REWRITTEN', 'DELETED']) {
      assertUnread(await inferReturn(marker), /built somewhere else/);
    }
  });

  it('never reads an envelope literal handed to a call as the const it initialised', async () => {
    assertUnread(await inferReturn('SAVED_INPUT'), /response helper that could not be followed/);
  });

  it('drops a member a helper fills only with error envelopes, and reads the rest', async () => {
    const inferred = await inferReturn('MEMBER_ERROR');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ scope: string; }');
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('answers unknown when the helper does not state an error status under the member', async () => {
    for (const marker of [
      'MEMBER_VARIABLE',
      'MEMBER_SUCCESS',
      'MEMBER_SECOND_HOP',
      'MEMBER_SPREAD',
      'MEMBER_SHORTHAND',
      'MEMBER_OPAQUE',
      'MEMBER_RECURSIVE',
      'MEMBER_WRITTEN',
      'MEMBER_ABSENT',
      'MEMBER_INSTALLED',
      'MEMBER_CALL_INPUT',
    ]) {
      assertUnread(await inferReturn(marker), /built somewhere else/);
    }
  });

  it('answers unknown for a domain value returned through a const beside an envelope', async () => {
    assertUnread(await inferReturn('ARTICLE_BESIDE_REPLY'), /built somewhere else/);
  });

  it('keeps today\'s answer for a domain value returned through a const', async () => {
    const found = await inferReturn('ARTICLE_CONST');
    assert.strictEqual(found.type_string, PINNED_ARTICLE_CONST);
    assert.strictEqual(found.any_provenance, undefined);
    const built = await inferReturn('ARTICLE_BUILT_CONST');
    assert.strictEqual(built.type_string, PINNED_ARTICLE_BUILT_CONST);
    assert.strictEqual(built.any_provenance, undefined);
  });

  it('answers unknown when the body is not produced by JSON.stringify', async () => {
    assertUnread(await inferReturn('CSV'), /not made by JSON\.stringify/);
  });

  it('answers unknown when the builder is an installed package', async () => {
    assertUnread(await inferReturn('PACKAGED'), /response helper that could not be followed/);
  });

  it('answers unknown when the serialised value is typed unknown', async () => {
    assertUnread(await inferReturn('UNKNOWN_PAYLOAD'), /typed any or unknown/);
  });

  it('never publishes string for a rule that reads the envelope body', async () => {
    const inferred = await inferReturn('LIST', ENVELOPE_RULE_CONFIG);
    assertNoEnvelope(inferred);
    assert.strictEqual(
      collapse(inferred.type_string),
      '{ notes: { id: string; title: string; }[]; }'
    );
  });

  it('answers unknown for a platform-typed value returned as-is', async () => {
    assertUnread(await inferReturn('PLATFORM_AS_IS'), /built somewhere else/);
  });

  it('keeps a returned call whose callback returns nothing as a branch', async () => {
    assertUnread(
      await inferReturn('FORWARD'),
      /response helper that could not be followed/
    );
  });

  it('answers unknown for a call handed two envelopes', async () => {
    assertUnread(
      await inferReturn('TWO_ENVELOPES'),
      /response helper that could not be followed/
    );
  });

  it('answers unknown when the serialised value holds any or unknown', async () => {
    for (const marker of ['INDEX_ANY', 'MEMBER_ANY', 'SEND_ANY', 'SEND_UNKNOWN']) {
      assertUnread(await inferReturn(marker), /typed any or unknown/);
    }
  });

  it('reads the argument when the builder takes object or a JSON value', async () => {
    const object = await inferReturn('OBJECT_PARAM');
    assert.strictEqual(collapse(object.type_string), '{ k: number; }');
    assert.strictEqual(object.is_explicit, false);
    const json = await inferReturn('JSON_PARAM');
    assert.strictEqual(collapse(json.type_string), '{ j: string; }');
    assert.strictEqual(json.is_explicit, false);
  });

  it('lets the inner envelope\'s own status outrank an outer numeric argument', async () => {
    const inferred = await inferReturn('TIMEOUT');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ late: boolean; }');
  });

  it('reads both the callback and the envelope a call is handed', async () => {
    const inferred = await inferReturn('FALLBACK');
    assertNoEnvelope(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ a: number; } | { b: string; }');
  });

  it('keeps today\'s answer when a serialised body is a call\'s input', async () => {
    const inferred = await inferReturn('STORE_JSON');
    assert.strictEqual(inferred.type_string, PINNED_STORE_JSON);
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('keeps today\'s answer for the repo\'s own all-optional type', async () => {
    const inferred = await inferReturn('PARTIAL');
    assert.strictEqual(inferred.type_string, PINNED_PARTIAL);
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('keeps today\'s answer for a domain type with status and body members', async () => {
    const inferred = await inferReturn('ARTICLE');
    assert.strictEqual(inferred.type_string, PINNED_ARTICLE);
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('keeps today\'s answer for an inferred literal whose body is not serialised', async () => {
    const inferred = await inferReturn('GREETING');
    assert.strictEqual(inferred.type_string, PINNED_GREETING);
    assert.strictEqual(inferred.any_provenance, undefined);
  });

  it('keeps today\'s answer when no return shows a serialised body', async () => {
    const inferred = await inferReturn('CSV_ONLY');
    assert.strictEqual(inferred.type_string, PINNED_CSV_ONLY);
    assert.strictEqual(inferred.any_provenance, undefined);
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
const PINNED_ARTICLE = '{ id: string; title: string; body: string; status: number; }';
const PINNED_GREETING = '{ status: number; body: string; }';
const PINNED_CSV_ONLY =
  '{ statusCode: number; headers?: { [x: string]: string; }; body: string; }';
const PINNED_STORE_JSON = '{ id: string; title: string; body: string; status: number; }';
/** Pinned from origin/main before carrick#2205, byte for byte. */
const PINNED_ARTICLE_CONST = '{ id: string; title: string; body: string; status: number; }';
const PINNED_ARTICLE_BUILT_CONST = '{ id: string; title: string; body: string; status: number; }';
const PINNED_PARTIAL =
  '{ id?: string | undefined; title?: string | undefined; body?: string | undefined; status?: number | undefined; }';
