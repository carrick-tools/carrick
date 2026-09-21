/**
 * Regression for carrick#1382: a json body read overrode the parse applied to
 * it, so a consumer that validates what it reads published `any`.
 *
 * `const parsed = parseEnvelope(await response.json())` states the payload
 * twice: once as the untyped read, once as the parser's declared result. The
 * def-use walk visits candidates in pre-order, so the `VariableDeclaration`
 * for `parsed` is reached BEFORE the identifier `response` inside its own
 * initializer — the body-read branch then overwrote the better answer with the
 * `any` of `response.json()`.
 *
 * The body read stays the floor it was built to be (carrick#1017): a cast
 * around it is still read off it, and a body read whose value goes nowhere
 * useful keeps its own answer. What changed is that a call which CONSUMES the
 * body read and whose own result the source binds states the payload more
 * precisely, so that call's result wins.
 *
 * Nothing here names a library or a validator: the shape is "the body read is
 * an argument of a call whose result is bound and whose type is an object",
 * which is a shape of the language.
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
declare function parseEnvelope(value: unknown): PreferenceEnvelope;
declare function isEnvelope(value: unknown): boolean;
declare function store(value: unknown): void;
declare function enqueue(value: unknown): { jobId: string };

export async function loadParsed(): Promise<PreferenceEnvelope> {
  const response = await request("/v1/me/preferences");
  const parsed = parseEnvelope(await response.json());
  return parsed;
}

export async function loadCast(): Promise<PreferenceEnvelope> {
  const response = await request("/v1/me/cast");
  return (await response.json()) as PreferenceEnvelope;
}

export async function loadChecked(): Promise<boolean> {
  const response = await request("/v1/me/checked");
  const ok = isEnvelope(await response.json());
  return ok;
}

export async function loadStored(): Promise<void> {
  const response = await request("/v1/me/stored");
  store(await response.json());
}

export async function loadQueued(): Promise<void> {
  const response = await request("/v1/me/queued");
  enqueue(await response.json());
}
`;

/** 1-based lines in SERVICE_TS, read off the source above. */
const PARSED_LINE = 18;
const CAST_LINE = 24;
const CHECKED_LINE = 29;
const STORED_LINE = 35;
const QUEUED_LINE = 40;

const ENVELOPE_TEXT =
  '{ flags: { [key: string]: boolean; }; list: string[]; version: string; }';

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1382: a body read is a floor, not a ceiling', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1382-'));
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

  it('publishes the parse applied to the body read, not the untyped read', async () => {
    const inferred = await infer(
      'Endpoint_Preferences_Response_Parsed',
      PARSED_LINE,
      'request("/v1/me/preferences")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.notStrictEqual(
      collapse(inferred.type_string),
      'any',
      'the parser states the payload; the untyped body read must not override it'
    );
    assert.strictEqual(collapse(inferred.type_string), 'PreferenceEnvelope');
  });

  it('still reads a cast off the body read itself', async () => {
    const inferred = await infer(
      'Endpoint_Cast_Response',
      CAST_LINE,
      'request("/v1/me/cast")'
    );
    assert.ok(inferred, 'the cast states the payload');
    assert.strictEqual(collapse(inferred.type_string), ENVELOPE_TEXT);
    assert.strictEqual(inferred.is_explicit, true);
  });

  it('keeps the body read where the call consuming it yields a primitive', async () => {
    // `isEnvelope(...)` answers `boolean`. A boolean is never a wire payload,
    // and publishing it would be a concrete-but-wrong contract — strictly
    // worse than the honest `any` of the read.
    const inferred = await infer(
      'Endpoint_Checked_Response',
      CHECKED_LINE,
      'request("/v1/me/checked")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'any');
  });

  it('keeps the body read where the consuming call\'s result goes nowhere', async () => {
    const inferred = await infer(
      'Endpoint_Stored_Response',
      STORED_LINE,
      'request("/v1/me/stored")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'any');
  });

  it('keeps the body read where an object result is discarded', async () => {
    // `enqueue(...)` answers an object, so the shape test alone would take it.
    // The source throws that object away, which is the tell that the call was
    // made for its effect: the receipt it hands back describes the queueing,
    // not the body that was queued.
    const inferred = await infer(
      'Endpoint_Queued_Response',
      QUEUED_LINE,
      'request("/v1/me/queued")'
    );
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'any');
  });
});
