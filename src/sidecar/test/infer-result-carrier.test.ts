/**
 * Regression for carrick#1376: a fetcher that answers a generic two-parameter
 * result carrier published the CARRIER as the consumer's response contract.
 *
 * `requestJson(path, parse): Future<Outcome<T, Error>>` states the payload in
 * its success type argument. The def-use walk has nothing to end on — a member
 * read is not a candidate — so the terminal stayed the binding and the row
 * published `{ ok: false; error: Error } | { ok: true; value: Envelope }`: an
 * envelope the transport uses to say whether it worked, not the body that
 * crossed the wire. The capture arm agreed with it, so the surface declared
 * the same carrier.
 *
 * The success argument is now read off the carrier structurally, and the
 * anchor with it, so the row states the payload and the surface pre-claims the
 * payload's own declaration.
 *
 * Nothing here matches a library or a package. The shape is "a union of object
 * branches, instantiated with type arguments, one of which a branch carries as
 * a member"; success is told from failure by the platform's own error shape,
 * and where that cannot tell them apart, by which argument the source itself
 * reads out. Where neither can, the carrier is left alone and the limit is
 * logged.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVICE_TS = `export interface PreferenceEnvelope {
  flags: { [key: string]: boolean };
  version: string;
}

type Outcome<T, E> = { ok: true; value: T } | { ok: false; error: E };

type Pair<A, B> = { first: true; a: A } | { first: false; b: B };

interface Attempt<T, E> {
  value: T;
  failure: E | null;
  tries: number;
}

type Flagged<T, E> = { ok: true; items: T[] } | { ok: false; errors: E[] };

interface Future<T> {
  then<R>(onValue: (value: T) => R): Future<R>;
}

declare function parseEnvelope(value: unknown): PreferenceEnvelope;
declare function requestJson<T>(
  path: string,
  parse: (value: unknown) => T
): Future<Outcome<T, Error>>;
declare function requestPair(path: string): Promise<Pair<PreferenceEnvelope, string>>;
declare function requestAttempt(path: string): Promise<Attempt<PreferenceEnvelope, Error>>;
declare function requestFlagged(path: string): Promise<Flagged<PreferenceEnvelope, Error>>;
declare function note(value: unknown): void;

export async function loadPreferences(): Promise<PreferenceEnvelope | null> {
  const outcome = await requestJson("/v1/me/preferences", parseEnvelope);
  if (!outcome.ok) {
    return null;
  }
  return outcome.value;
}

export function fetchPreferences(): Future<Outcome<PreferenceEnvelope, Error>> {
  return requestJson("/v1/me/fetch", parseEnvelope);
}

export async function loadPair(): Promise<void> {
  const pair = await requestPair("/v1/me/pair");
  note(pair);
}

export async function loadPairRead(): Promise<PreferenceEnvelope | null> {
  const pair = await requestPair("/v1/me/pair-read");
  if (!pair.first) {
    return null;
  }
  return pair.a;
}

export async function loadAttempt(): Promise<PreferenceEnvelope> {
  const attempt = await requestAttempt("/v1/me/attempt");
  return attempt.value;
}

export async function loadFlagged(): Promise<PreferenceEnvelope[]> {
  const flagged = await requestFlagged("/v1/me/flagged");
  if (!flagged.ok) {
    return [];
  }
  return flagged.items;
}
`;

/** 1-based lines in SERVICE_TS, read off the source above. */
const AWAITED_LINE = 33;
const RETURNED_LINE = 41;
const AMBIGUOUS_LINE = 45;
const READ_LINE = 50;
const ATTEMPT_LINE = 58;
const FLAGGED_LINE = 63;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    primary_type_symbol?: string;
    array_depth?: number;
    any_provenance?: Array<{ path: string; kind: string; reason: string }>;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

/** The payload printed with its own members, as #257 requires of this path. */
const ENVELOPE_TEXT =
  '{ flags: { [key: string]: boolean; }; version: string; }';

describe('carrick#1376: a result carrier is not the payload it carries', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1376-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
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

  /** The locator a scan really sends: the model's expression text and its line. */
  async function infer(alias: string, line: number, expressionText: string) {
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
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('resolves an awaited two-parameter carrier to its success type argument', async () => {
    const inferred = await infer(
      'Endpoint_Preferences_Response',
      AWAITED_LINE,
      'requestJson("/v1/me/preferences", parseEnvelope)'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.ok(
      !/\bok:\s*(true|false)\b/.test(inferred.type_string),
      `the carrier says whether it worked, not what crossed the wire, got: ${inferred.type_string}`
    );
    assert.strictEqual(collapse(inferred.type_string), ENVELOPE_TEXT);
    assert.strictEqual(
      inferred.primary_type_symbol,
      'PreferenceEnvelope',
      'the anchor must name the payload, or the surface pre-claims the carrier'
    );
  });

  it('resolves a carrier the fetcher returns without awaiting it', async () => {
    // The thenable is peeled structurally (the language's await protocol, read
    // off the `then` signature), so a promise-like of the source's own making
    // is unwrapped exactly as a `Promise` is.
    const inferred = await infer(
      'Endpoint_Fetch_Response',
      RETURNED_LINE,
      'requestJson("/v1/me/fetch", parseEnvelope)'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), ENVELOPE_TEXT);
  });

  it('leaves a carrier alone when nothing tells its success side from its failure side', async () => {
    // `Pair<PreferenceEnvelope, string>`: neither argument is error-shaped and
    // the source reads neither out. Guessing here would publish a coin flip as
    // a contract, so the carrier keeps its own answer and the limit is logged.
    const inferred = await infer(
      'Endpoint_Pair_Response',
      AMBIGUOUS_LINE,
      'requestPair("/v1/me/pair")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.match(inferred.type_string, /first:/);
  });

  it('uses the argument the source reads where the error shape cannot decide', async () => {
    const inferred = await infer(
      'Endpoint_PairRead_Response',
      READ_LINE,
      'requestPair("/v1/me/pair-read")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), ENVELOPE_TEXT);
  });

  it('leaves a single generic object alone: a carrier is a union of outcomes', async () => {
    // `Attempt<T, E>` holds the value and the failure side by side rather than
    // as alternatives. A single generic object read member by member is
    // carrick#1375's question, not this one, and it answers: the site states a
    // part of a payload, so it abstains and a sibling site answers the alias.
    // Widening this rule past unions would overrule that decision.
    const inferred = await infer(
      'Endpoint_Attempt_Response',
      ATTEMPT_LINE,
      'requestAttempt("/v1/me/attempt")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
  });

  it('leaves a carrier whose branches hold the argument only inside another shape', async () => {
    // `{ ok: true; items: T[] }`: the branch carries `T[]`, not `T`. Reading
    // the argument out here would publish an element where a LIST crossed the
    // wire, which is a concrete answer of the wrong arity.
    const inferred = await infer(
      'Endpoint_Flagged_Response',
      FLAGGED_LINE,
      'requestFlagged("/v1/me/flagged")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.match(inferred.type_string, /items/);
  });
});
