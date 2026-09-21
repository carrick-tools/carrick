/**
 * Regression for carrick#1375: a value DERIVED from a call's result was
 * published as the expected response type of that call.
 *
 * A client declares an envelope, parses exactly that envelope out of the
 * transport response, and a state hook then reads one member of it
 * (`query.data?.flags`). The def-use walk behind `call_result` ended on that
 * member read, so the consumer's expected response type was recorded as the
 * projection — a boolean map — and the producer's real envelope was reported
 * as not assignable to it. Both sides were right; the type read was not the
 * contract either of them states.
 *
 * The expected wire type of a consumer comes from the request boundary: the
 * call's own result, a wrapper rule's payload, or the body parsed out of the
 * transport response. Never a value computed downstream of it. Where every
 * read of the result takes a MEMBER out of it, the site states no payload of
 * its own and the row abstains, so a sibling site for the same operation (the
 * fetcher, which does state one) answers the alias instead.
 *
 * Nothing here matches a library, a hook name or a package: the walk's
 * classification is `identifier is the receiver of a member access`, which is
 * a shape of the language.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVICE_TS = `export interface PreferenceEnvelope {
  flags: { [key: string]: boolean };
  list: string[];
  version: string;
}

interface TransportBody {
  json(): Promise<any>;
}

declare function request(path: string): Promise<TransportBody>;
declare function readRaw(path: string): Promise<unknown>;
declare function parseEnvelope(value: unknown): PreferenceEnvelope;
declare function store(value: unknown): void;

export async function loadPreferences(): Promise<PreferenceEnvelope> {
  const response = await request("/v1/me/preferences");
  return (await response.json()) as PreferenceEnvelope;
}

export async function loadParsedPreferences(): Promise<PreferenceEnvelope> {
  const raw = await readRaw("/v1/me/preferences");
  const parsed = parseEnvelope(raw);
  return parsed;
}

export const preferencesApi = {
  getMine: (): Promise<PreferenceEnvelope> => loadPreferences(),
};

interface ResourceState<T> {
  data: T | undefined;
  isPending: boolean;
  refresh(): void;
}

declare function useResource<T>(options: {
  key: string;
  load: () => Promise<T>;
}): ResourceState<T>;

export function usePreferences() {
  const query = useResource({ key: "prefs", load: () => preferencesApi.getMine() });
  const flags = query.data?.flags ?? {};
  return { flags, pending: query.isPending };
}

type SendOutcome = { ok: true } | { ok: false; reason: string };

declare function sendConfirmation(address: string): Promise<SendOutcome>;

export async function confirm(address: string): Promise<boolean> {
  const sent = await sendConfirmation(address);
  if (!sent.ok) {
    return false;
  }
  return true;
}

export async function cachePreferences(): Promise<PreferenceEnvelope> {
  const envelope = await loadPreferences();
  const size = envelope.list.length;
  store(size);
  return envelope;
}
`;

/** 1-based lines in SERVICE_TS, read off the source above. */
const FETCHER_LINE = 17;
const PARSED_FETCHER_LINE = 22;
const HOOK_LINE = 43;
const NON_GENERIC_LINE = 53;
const WHOLE_READ_LINE = 61;

const ENVELOPE_TEXT =
  '{ flags: { [key: string]: boolean; }; list: string[]; version: string; }';

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

describe('carrick#1375: a projection of the response is not the response', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1375-'));
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

  it('abstains where every read of the call result takes a member out of it', async () => {
    const inferred = await infer(
      'Endpoint_Preferences_Response_Hook',
      HOOK_LINE,
      'useResource({ key: "prefs", load: () => preferencesApi.getMine() })'
    );
    assert.ok(inferred, 'the row must be answered, with a decision on it');
    assert.ok(
      !/boolean/.test(inferred.type_string),
      `the projection the hook derives is not the wire payload, got: ${inferred.type_string}`
    );
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
    assert.strictEqual(inferred.is_explicit, false);
    // `inference_was_blind` (Rust) reads a bare top type WITH no anchor and no
    // depth. An anchor here would make the abstain read as a sighted answer and
    // keep a symbol anchor the array-ness of which nothing witnessed.
    assert.strictEqual(inferred.primary_type_symbol, undefined);
    assert.strictEqual(inferred.array_depth, undefined);
    const root = (inferred.any_provenance ?? []).filter((p) => p.path === '');
    assert.deepStrictEqual(
      root.map((p) => p.reason),
      ['projected_value_only'],
      'the decision must ride the row, or the capture re-runs the raw locator and ' +
        'publishes the projection this just declined to publish'
    );
  });

  it('reads the parsed body out of the transport response at the request boundary', async () => {
    const inferred = await infer(
      'Endpoint_Preferences_Response_Fetcher',
      FETCHER_LINE,
      'request("/v1/me/preferences")'
    );
    assert.ok(inferred, 'the fetcher states the contract and must publish it');
    assert.strictEqual(collapse(inferred.type_string), ENVELOPE_TEXT);
  });

  it('keeps following the walk to a parsed value, which is not a projection', async () => {
    const inferred = await infer(
      'Endpoint_Preferences_Response_Parsed',
      PARSED_FETCHER_LINE,
      'readRaw("/v1/me/preferences")'
    );
    assert.ok(inferred, 'a parser states the payload and must publish it');
    assert.strictEqual(
      collapse(inferred.type_string),
      'PreferenceEnvelope',
      'a parsed value is derived, but it is the payload itself and not a part of it'
    );
  });

  it('answers the declared result where the call is not a generic envelope', async () => {
    // The live shape this rule had to be narrowed for: a result read only
    // through its members (`sent.ok`), whose declared type IS the payload.
    // A generic is what marks an envelope the source unwraps by hand; without
    // one, the call states the contract and abstaining would discard it.
    const inferred = await infer(
      'Endpoint_Confirm_Response',
      NON_GENERIC_LINE,
      'sendConfirmation(address)'
    );
    assert.ok(inferred, 'the declared result is the contract here');
    assert.notStrictEqual(
      collapse(inferred.type_string),
      'unknown',
      'a non-generic result read member by member still states its own payload'
    );
    assert.match(inferred.type_string, /ok/);
  });

  it('answers the payload where the value is also read whole', async () => {
    const inferred = await infer(
      'Endpoint_Preferences_Response_Whole',
      WHOLE_READ_LINE,
      'loadPreferences()'
    );
    assert.ok(inferred, 'the payload is stated here and must be published');
    assert.strictEqual(
      collapse(inferred.type_string),
      'PreferenceEnvelope',
      'a member read beside a whole read is incidental: the payload is the call result'
    );
  });

  it('answers the inner request call on the hook line as the payload it yields', async () => {
    // The same line carries two calls. The one that IS the request boundary
    // answers the envelope; only the wrapper around it abstains.
    const inferred = await infer(
      'Endpoint_Preferences_Response_Inner',
      HOOK_LINE,
      'preferencesApi.getMine()'
    );
    assert.ok(inferred, 'the inner call states the payload');
    assert.strictEqual(collapse(inferred.type_string), 'PreferenceEnvelope');
  });
});
