/**
 * carrick#1851: a body read taken in place on the call's own value is the
 * call's payload, as the same read on a binding of that value is.
 *
 * `const res = await fetch(url); const body = await res.text()` has always
 * ended the def-use walk on the read. Written without the binding,
 * `const body = await (await fetch(url)).text()`, the declaration's
 * initializer is the read and not the call, so no binding was found, the walk
 * had nothing to follow, and the row published the transport response object.
 *
 * The read is now the terminal where its receiver is the call itself, through
 * the wrappers that leave a value as it is (parentheses, `await`, `!`). What
 * the binding walk does with a read then holds here too: a call that takes
 * the read and states what it returns says more than the read (carrick#1382),
 * and a text read publishes `string` marked as raw text, which is what keeps
 * it from being judged against a JSON body (carrick#1842).
 *
 * The forms that were already right keep their answers: a cast around the
 * read, an annotated declaration, a returned read. A member that is not a
 * whole-body read is not taken.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVICE_TS = `export interface Receipt {
  id: string;
  settled: boolean;
}

declare function parseReceipt(value: unknown): Receipt;

/** A request that is a promise and also reads its own body. */
interface PendingReply extends Promise<{ status: number }> {
  json<T = unknown>(): Promise<T>;
  text(): Promise<string>;
}
declare function send(url: string): PendingReply;

export async function chainedJson() {
  const body = await (await fetch("/chain/json")).json();
  return body;
}

export async function chainedText() {
  const receipt = await (await fetch("/chain/text")).text();
  return receipt;
}

export async function chainedNonNull() {
  const receipt = await (await fetch("/chain/non-null"))!.text();
  return receipt;
}

export async function chainedDiscarded() {
  console.log(await (await fetch("/chain/discarded")).text());
}

export async function chainedParsed() {
  const receipt = parseReceipt(await (await fetch("/chain/parsed")).json());
  return receipt;
}

export async function chainedParsedText() {
  const receipt: Receipt = JSON.parse(await (await fetch("/chain/parsed-text")).text());
  return receipt;
}

export async function directJson() {
  const receipt = await send("/chain/direct-json").json<Receipt>();
  return receipt;
}

export async function directText() {
  const receipt = await send("/chain/direct-text").text();
  return receipt;
}

export async function chainedCast(): Promise<Receipt> {
  const receipt = (await (await fetch("/chain/cast")).json()) as Receipt;
  return receipt;
}

export async function chainedAnnotated(): Promise<Receipt> {
  const receipt: Receipt = await (await fetch("/chain/annotated")).json();
  return receipt;
}

export async function returnedJson() {
  return (await fetch("/chain/returned-json")).json();
}

export async function returnedText() {
  return (await fetch("/chain/returned-text")).text();
}

export async function chainedStatus() {
  const status = (await fetch("/chain/status")).status;
  return status;
}

export async function boundJson() {
  const res = await fetch("/bound/json");
  const body = await res.json();
  return body;
}

export async function boundText() {
  const res = await fetch("/bound/text");
  const receipt = await res.text();
  return receipt;
}

export async function boundParsed() {
  const res = await fetch("/bound/parsed");
  const receipt = parseReceipt(await res.json());
  return receipt;
}

export async function boundCast(): Promise<Receipt> {
  const res = await fetch("/bound/cast");
  const receipt = (await res.json()) as Receipt;
  return receipt;
}
`;

interface Inferred {
  alias: string;
  type_string: string;
  is_explicit: boolean;
  primary_type_symbol?: string;
  raw_text_read?: boolean;
  stated_body?: unknown;
}

interface InferShape {
  inferred_types?: Inferred[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe("carrick#1851: a body read taken in place on the call's value is the terminal", () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1851-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
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

  /**
   * Infer the call to `url`, with the locator a scan sends: the call's own
   * text (`fetch("/chain/json")`) and its line.
   */
  async function infer(url: string): Promise<Inferred> {
    const lines = SERVICE_TS.split('\n');
    const hits = lines.flatMap((line, index) => (line.includes(`("${url}")`) ? [index] : []));
    assert.strictEqual(hits.length, 1, `${url} must name one call`);
    const expression = lines[hits[0]].match(/(fetch|send)\("[^"]+"\)/)?.[0];
    assert.ok(expression, `${url} must be a call`);
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: url,
      requests: [
        {
          file_path: servicePath,
          line_number: hits[0] + 1,
          infer_kind: 'call_result',
          alias: url,
          expression_text: expression,
          expression_line: hits[0] + 1,
        },
      ],
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === url);
    assert.ok(inferred, 'the row must be answered');
    return inferred;
  }

  describe('the read is the terminal, chained and bound, uncast', () => {
    it('a json read publishes what the read yields, not the response object', async () => {
      const inferred = await infer('/chain/json');
      assert.strictEqual(collapse(inferred.type_string), 'any');
      assert.strictEqual(inferred.raw_text_read, undefined, 'a json read is not raw text');
    });

    it('a text read publishes string, marked as raw text', async () => {
      const inferred = await infer('/chain/text');
      assert.strictEqual(collapse(inferred.type_string), 'string');
      assert.strictEqual(
        inferred.raw_text_read,
        true,
        'unmarked, the string is judged against the other side as a JSON body'
      );
    });

    it('answers as the same reads do on a binding of the response', async () => {
      for (const form of ['json', 'text']) {
        const chained = await infer(`/chain/${form}`);
        const bound = await infer(`/bound/${form}`);
        assert.deepStrictEqual(
          [chained.type_string, chained.is_explicit, chained.raw_text_read, chained.primary_type_symbol],
          [bound.type_string, bound.is_explicit, bound.raw_text_read, bound.primary_type_symbol],
          `${form}: eliding the binding must not change the answer`
        );
      }
    });
  });

  describe('the receiver is the call itself, through wrappers that leave a value as it is', () => {
    it('reads through a non-null assertion on the awaited call', async () => {
      const inferred = await infer('/chain/non-null');
      assert.strictEqual(collapse(inferred.type_string), 'string');
      assert.strictEqual(inferred.raw_text_read, true);
    });

    it('takes a read whose value is handed on unbound', async () => {
      const inferred = await infer('/chain/discarded');
      assert.strictEqual(collapse(inferred.type_string), 'string');
      assert.strictEqual(inferred.raw_text_read, true);
    });

    it('takes a read on a call that is not awaited first', async () => {
      // The request is a promise that reads its own body: the read's receiver
      // is still the call's own value, and what it yields is the payload.
      const json = await infer('/chain/direct-json');
      assert.strictEqual(collapse(json.type_string), 'Receipt');
      assert.strictEqual(json.raw_text_read, undefined);
      const text = await infer('/chain/direct-text');
      assert.strictEqual(collapse(text.type_string), 'string');
      assert.strictEqual(text.raw_text_read, true);
    });
  });

  describe('what the binding walk does with a read holds here too', () => {
    it('a call that takes the json read and states what it returns says more', async () => {
      const chained = await infer('/chain/parsed');
      const bound = await infer('/bound/parsed');
      assert.strictEqual(collapse(chained.type_string), 'Receipt');
      assert.strictEqual(chained.type_string, bound.type_string);
      assert.strictEqual(chained.raw_text_read, undefined);
    });

    it('a text read the source parses into a type it states is not raw text', async () => {
      const inferred = await infer('/chain/parsed-text');
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; settled: boolean; }');
      assert.strictEqual(inferred.is_explicit, true);
      assert.strictEqual(inferred.raw_text_read, undefined);
    });

    it('a cast at the read reports the body it states, as it does on a binding', async () => {
      const chained = await infer('/chain/cast');
      const bound = await infer('/bound/cast');
      assert.ok(bound.stated_body, 'the bound cast states its body');
      assert.deepStrictEqual(chained.stated_body, bound.stated_body);
    });
  });

  describe('the forms that were already right do not move', () => {
    it('a cast around the read', async () => {
      const inferred = await infer('/chain/cast');
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; settled: boolean; }');
      assert.strictEqual(inferred.is_explicit, true);
      assert.strictEqual(inferred.raw_text_read, undefined);
    });

    it('an annotated declaration', async () => {
      const inferred = await infer('/chain/annotated');
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; settled: boolean; }');
      assert.strictEqual(inferred.is_explicit, true);
    });

    it('a returned json read', async () => {
      const inferred = await infer('/chain/returned-json');
      assert.strictEqual(collapse(inferred.type_string), 'any');
      assert.strictEqual(inferred.raw_text_read, undefined);
    });

    it('a returned text read, marked', async () => {
      const inferred = await infer('/chain/returned-text');
      assert.strictEqual(collapse(inferred.type_string), 'string');
      assert.strictEqual(inferred.raw_text_read, true);
    });

    it('a member that is not a whole-body read is not taken', async () => {
      // `(await fetch(url)).status` reads a part of the response. The row
      // keeps the answer it had; this fix reads bodies, not members.
      const inferred = await infer('/chain/status');
      assert.strictEqual(collapse(inferred.type_string), 'Response');
    });
  });
});
