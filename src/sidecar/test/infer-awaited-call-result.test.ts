/**
 * carrick#1877: what a call's result holds is what `await` yields for the
 * call's type, read off the language's await protocol and not off a name.
 *
 * A call typed `Promise<T>` or `PromiseLike<T>` has always published `T` and
 * anchored its row on `T`: the two were peeled by name. A class that extends
 * `Promise`, or one that only has a `then`, has a symbol of its own, so the
 * name test missed it: the row was anchored on the class, and a call
 * returned without `await` published the class itself, which is transport.
 *
 * The protocol reading was already there for a result carrier behind a
 * thenable of the source's own making (carrick#1376), and it did not reach
 * this case for two reasons. It was only asked on the way to a carrier, so
 * its answer was dropped where no carrier was behind the thenable. And it
 * read `then`'s first parameter as a callback as written, which the
 * platform's own `then` is not: a subclass of `Promise` inherits
 * `onfulfilled?: ((value: T) => ...) | null`, an optional, nullable callback.
 *
 * One reading now answers both the published text and the anchor, and it is
 * the one a carrier is found through. What it yields is `then`'s value, so a
 * subclass whose `then` yields something other than its type argument
 * publishes what it yields. A property called `then` that cannot be called
 * is no protocol, and the type is left as it is.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const CLIENT_TS = `export interface Invoice {
  id: string;
  total: number;
}

/** A subclass of Promise with a member of its own, as a request library writes one. */
export class PendingCall<T> extends Promise<T> {
  withResponse(): Promise<{ data: T; status: number }> {
    return this.then((data) => ({ data, status: 200 }));
  }
}

/** Only a \`then\`; does not extend Promise. */
export class Deferred<T> {
  then(onDone: (value: T) => void): void {}
}

/** A subclass whose \`then\` yields something other than its type argument. */
export class Tagged<T> extends Promise<{ value: T; tag: string }> {}

/** Two \`then\` overloads; the first one's callback is not the one \`await\` takes. */
export class Odd<T> {
  then(onLabel: (label: string) => void, mode: "label"): void;
  then(onDone: (value: T) => void): void;
  then(..._args: unknown[]): void {}
}

/** A property called \`then\` that is not callable. */
export interface Step<T> {
  then: string;
  value: T;
}

export type Outcome<A, E> = { ok: true; value: A } | { ok: false; error: E };
export class ApiError extends Error {
  code = 0;
}
export interface Envelope<T> {
  data: T;
  requestId: string;
}

declare function request<T>(url: string): PendingCall<T>;
declare function defer<T>(url: string): Deferred<T>;
declare function tagged<T>(url: string): Tagged<T>;
declare function step<T>(url: string): Step<T>;
declare function odd<T>(url: string): Odd<T>;
declare function plain<T>(url: string): Promise<T>;
declare function like<T>(url: string): PromiseLike<T>;
declare function plainTagged<T>(url: string): Promise<{ value: T; tag: string }>;
declare function openRequest(url: string): PendingCall<Outcome<unknown, ApiError>>;
declare function openDefer(url: string): Deferred<Outcome<unknown, ApiError>>;

export async function awaited(id: string) {
  const invoice = await request<Invoice>(\`/awaited/\${id}\`);
  return invoice;
}

export function returned(id: string) {
  return request<Invoice>(\`/returned/\${id}\`);
}

export async function withResponse(id: string) {
  const reply = await request<Invoice>(\`/with-response/\${id}\`).withResponse();
  return reply.data;
}

export function list(id: string) {
  return request<Invoice[]>(\`/list/\${id}\`);
}

export function deferred(id: string) {
  return defer<Invoice>(\`/deferred/\${id}\`);
}

export async function awaitedDeferred(id: string) {
  const invoice = await defer<Invoice>(\`/awaited-deferred/\${id}\`);
  return invoice;
}

export function taggedCall(id: string) {
  return tagged<Invoice>(\`/tagged/\${id}\`);
}

export function plainTaggedCall(id: string) {
  return plainTagged<Invoice>(\`/plain-tagged/\${id}\`);
}

export function oddCall(id: string) {
  return odd<Invoice>(\`/odd/\${id}\`);
}

export function stepCall(id: string) {
  return step<Invoice>(\`/step/\${id}\`);
}

export function plainReturned(id: string) {
  return plain<Invoice>(\`/plain-returned/\${id}\`);
}

export async function plainAwaited(id: string) {
  const invoice = await plain<Invoice>(\`/plain-awaited/\${id}\`);
  return invoice;
}

export function likeReturned(id: string) {
  return like<Invoice>(\`/like-returned/\${id}\`);
}

export function carried(id: string) {
  return request<Outcome<Invoice, ApiError>>(\`/carried/\${id}\`);
}

export function plainCarried(id: string) {
  return plain<Outcome<Invoice, ApiError>>(\`/plain-carried/\${id}\`);
}

export function openCarried(id: string) {
  return openRequest(\`/open-carried/\${id}\`);
}

export function openDeferred(id: string) {
  return openDefer(\`/open-deferred/\${id}\`);
}

export function enveloped(id: string) {
  return request<Envelope<Invoice>>(\`/enveloped/\${id}\`);
}

export function plainEnveloped(id: string) {
  return plain<Envelope<Invoice>>(\`/plain-enveloped/\${id}\`);
}
`;

interface Inferred {
  alias: string;
  type_string: string;
  is_explicit: boolean;
  primary_type_symbol?: string;
  primary_type_symbol_source?: string;
  array_depth?: number;
}

interface InferShape {
  inferred_types?: Inferred[];
}

/** A rule for the service's own envelope: a name, no origin. */
const ENVELOPE_RULE = {
  rules: [{ wrapperSymbols: ['Envelope'], payloadGenericIndex: 0, unwrapRecursively: true }],
};

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe("carrick#1877: a call's result holds what await yields for the call's type", () => {
  let client: SidecarClient;
  let repoDir: string;
  let clientPath: string;

  before(async () => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1877-')));
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
    clientPath = path.join(repoDir, 'src', 'client.ts');
    fs.writeFileSync(clientPath, CLIENT_TS);

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
   * text and its line, and the service's rules when given.
   */
  async function infer(url: string, config?: typeof ENVELOPE_RULE): Promise<Inferred> {
    const lines = CLIENT_TS.split('\n');
    const hits = lines.flatMap((line, index) => (line.includes(`(\`${url}/`) ? [index] : []));
    assert.strictEqual(hits.length, 1, `${url} must name one call`);
    const expression = lines[hits[0]].match(/\w+(<[^(]*>)?\(`[^`]+`\)/)?.[0];
    assert.ok(expression, `${url} must be a call`);
    const alias = `${url}${config ? ':rules' : ''}`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: clientPath,
          line_number: hits[0] + 1,
          infer_kind: 'call_result',
          alias,
          expression_text: expression,
          expression_line: hits[0] + 1,
        },
      ],
      ...(config ? { extraction_config: config } : {}),
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, `${alias} must be answered`);
    return inferred;
  }

  /** What a row publishes and anchors on, for a comparison between two forms. */
  const answer = (inferred: Inferred) => [
    collapse(inferred.type_string),
    inferred.primary_type_symbol,
    inferred.primary_type_symbol_source,
    inferred.array_depth,
  ];

  describe('a subclass of Promise', () => {
    it('anchors an awaited call on the payload, not on the class', async () => {
      const inferred = await infer('/awaited');
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol_source, clientPath);
    });

    it('publishes the payload of a call returned without await, and anchors on it', async () => {
      const inferred = await infer('/returned');
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it('keeps the array levels of what await yields', async () => {
      const inferred = await infer('/list');
      assert.strictEqual(collapse(inferred.type_string), 'Invoice[]');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
      assert.strictEqual(inferred.array_depth, 1);
    });

    it("publishes the call's payload where the caller reads it through a member of the class", async () => {
      // `request(...).withResponse()`: the located call is `request(...)`,
      // and nothing awaits or binds it. Its payload is still what awaiting
      // it yields. `withResponse()` is the class handing that payload over
      // beside a status, so its result is an envelope the client makes
      // around the body, and a status is not a member the producer sends.
      const inferred = await infer('/with-response');
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it('answers as a plain Promise does, returned and awaited', async () => {
      assert.deepStrictEqual(answer(await infer('/returned')), answer(await infer('/plain-returned')));
      assert.deepStrictEqual(answer(await infer('/awaited')), answer(await infer('/plain-awaited')));
    });

    it('answers as a plain Promise does around an envelope, with and without its rule', async () => {
      assert.deepStrictEqual(answer(await infer('/enveloped')), answer(await infer('/plain-enveloped')));
      assert.deepStrictEqual(
        answer(await infer('/enveloped', ENVELOPE_RULE)),
        answer(await infer('/plain-enveloped', ENVELOPE_RULE))
      );
    });

    it('is looked through for a result carrier, as a plain Promise is', async () => {
      const inferred = await infer('/carried');
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; total: number; }');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
      assert.deepStrictEqual(answer(inferred), answer(await infer('/plain-carried')));
    });
  });

  describe('a carrier behind the thenable whose payload is open', () => {
    // The carrier read takes no payload out of `Outcome<unknown, ApiError>`,
    // so the row keeps the result as written (carrick#1376), thenable and
    // all. The reading that looked through the thenable found a carrier, and
    // neither the thenable nor the carrier is the row's type to anchor on.
    for (const [url, written] of [
      ['/open-carried', 'PendingCall<Outcome<unknown, ApiError>>'],
      ['/open-deferred', 'Deferred<Outcome<unknown, ApiError>>'],
    ] as const) {
      it(`keeps ${written} as written and anchors nothing`, async () => {
        const inferred = await infer(url);
        assert.strictEqual(collapse(inferred.type_string), written);
        assert.strictEqual(inferred.primary_type_symbol, undefined);
        assert.strictEqual(inferred.primary_type_symbol_source, undefined);
      });
    }
  });

  describe('a class that only has a then', () => {
    it('publishes and anchors on what its then yields, returned', async () => {
      const inferred = await infer('/deferred');
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it('anchors an awaited call on what its then yields', async () => {
      const inferred = await infer('/awaited-deferred');
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });
  });

  describe('the protocol, not the type argument', () => {
    it('publishes what then yields where that is not the type argument', async () => {
      // `Tagged<Invoice>` extends `Promise<{ value: Invoice; tag: string }>`.
      // Awaiting it yields that object; `Invoice` is only its argument, and
      // a reading that took the argument would publish a part of the body.
      const inferred = await infer('/tagged');
      assert.strictEqual(collapse(inferred.type_string), '{ value: Invoice; tag: string; }');
      assert.notStrictEqual(inferred.primary_type_symbol, 'Tagged');
      // The row answers as a plain Promise of that object does, anchor
      // included: this reading decides nothing a `Promise<T>` did not.
      assert.deepStrictEqual(answer(inferred), answer(await infer('/plain-tagged')));
    });

    it('leaves a then it cannot read the way the compiler does as it is', async () => {
      // `Odd` overloads `then`, and the first signature's callback takes a
      // label. Awaiting an `Odd<Invoice>` does not yield `string`, and the
      // compiler says so; the row keeps the type as written rather than
      // publish the first signature's parameter.
      const inferred = await infer('/odd');
      assert.strictEqual(collapse(inferred.type_string), 'Odd<Invoice>');
    });

    it('leaves a type whose then cannot be called as it is', async () => {
      const inferred = await infer('/step');
      assert.strictEqual(collapse(inferred.type_string), 'Step<Invoice>');
      assert.strictEqual(inferred.primary_type_symbol, 'Step');
    });
  });

  describe('what does not move', () => {
    it('a plain Promise, returned and awaited', async () => {
      for (const url of ['/plain-returned', '/plain-awaited']) {
        const inferred = await infer(url);
        assert.strictEqual(collapse(inferred.type_string), 'Invoice');
        assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
      }
    });

    it('a PromiseLike', async () => {
      const inferred = await infer('/like-returned');
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it('a result carrier behind a plain Promise', async () => {
      const inferred = await infer('/plain-carried');
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; total: number; }');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it('an envelope behind a plain Promise, with and without its rule', async () => {
      const bare = await infer('/plain-enveloped');
      assert.strictEqual(collapse(bare.type_string), 'Envelope<Invoice>');
      assert.strictEqual(bare.primary_type_symbol, 'Envelope');
      const ruled = await infer('/plain-enveloped', ENVELOPE_RULE);
      assert.strictEqual(collapse(ruled.type_string), 'Envelope<Invoice>');
      assert.strictEqual(ruled.primary_type_symbol, 'Invoice');
    });
  });
});
