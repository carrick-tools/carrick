/**
 * Regression for carrick#1749: two consumer call sites recorded an expected
 * response type that is not the body the caller reads.
 *
 *  (a) The source casts the body read to a wrapper (`res.json() as
 *      Promise<{ members: Member[] }>`) and the model named the element
 *      (`Member`). The inference reads the cast correctly, but nothing told
 *      the scanner that the text was the source's own statement, so the
 *      model's symbol won and the pair read incompatible. The inference now
 *      says so (`stated_body`), with the name the statement is rooted at:
 *      none for an object literal type, `Member` for `Promise<Member>`.
 *
 *  (b) The call starts a chain, and a callback in it is handed the body
 *      unread and casts it (`.mapOk(response => { const data = response as
 *      SearchResponse; ... })`). The call sits inside a `return`, so the walk
 *      answered the value the chain ends in, which is what the caller
 *      computed from the body. The cast is the body read.
 *
 * Nothing here matches a library or a method name. The chain is "member calls
 * whose receiver is the previous call"; the body read is "a callback's first
 * parameter, typed `unknown` by the compiler, cast by the source", and it
 * answers only when exactly one exists in the chain.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVICE_TS = `export type Member = { id: string; name: string; email: string };

interface Reply {
  ok: boolean;
  status: number;
  json(): Promise<any>;
}

declare function fetchReply(path: string): Promise<Reply>;

export const listMembers = async (): Promise<{ members: Member[] }> => {
  const response = await fetchReply("/v1/members");
  if (!response.ok) {
    throw new Error("members " + response.status);
  }
  return response.json() as Promise<{ members: Member[] }>;
};

export const getMember = async (): Promise<Member> => {
  const response = await fetchReply("/v1/member");
  return response.json() as Promise<Member>;
};

export const findMember = async (): Promise<Member | null> => {
  const response = await fetchReply("/v1/member-or-none");
  const found: Member[] | null = await response.json();
  return found ? found[0] : null;
};

type Loader = () => Promise<unknown>;

export const loadLoose: Loader = async () => {
  const response = await fetchReply("/v1/loose");
  return response.json();
};

declare class Chain<T> {
  check(test: (value: T) => boolean): Chain<T>;
  mapOk<R>(transform: (value: T) => R): Chain<R>;
  mapFailure(transform: (failure: unknown) => Error): Chain<T>;
  peek(observe: (value: any) => void): Chain<T>;
}

declare function request(url: string): Chain<unknown>;
declare function hasBody(value: unknown): boolean;

type SearchHit = { registrationNumber: string; name: string; city?: string };
type SearchResponse = { results: SearchHit[]; messageKey?: string };
export type Suggestions = { suggestions: { value: string; label: string }[] };

export const search = (query: string): Chain<Suggestions> => {
  return request(\`/v1/search?q=\${query}\`)
    .check(hasBody)
    .mapOk((response) => {
      const data = response as SearchResponse;
      return {
        suggestions: data.results.map((hit) => ({
          value: hit.registrationNumber,
          label: hit.name,
        })),
      };
    });
};

export const searchUncast = (query: string): Chain<Suggestions> => {
  return request(\`/v1/search-uncast?q=\${query}\`).mapOk((response) => {
    const label = String(response) as string;
    void (response as unknown);
    return { suggestions: [{ value: label, label: query }] };
  });
};

export const searchTwoCasts = (query: string): Chain<number> => {
  return request(\`/v1/search-two?q=\${query}\`)
    .mapFailure((failure) => failure as Error)
    .mapOk((response) => (response as SearchResponse).results.length);
};

export const searchAny = (query: string): Chain<unknown> => {
  return request(\`/v1/search-any?q=\${query}\`).peek((value) => {
    const seen = value as SearchResponse;
    void seen;
  });
};
`;

/** 1-based line of the first line of SERVICE_TS that contains `marker`. */
function lineOf(marker: string): number {
  const index = SERVICE_TS.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `marker not in the fixture: ${marker}`);
  return index + 1;
}

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    is_explicit: boolean;
    stated_body?: { root?: string; root_source?: string; array_depth?: number };
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

const SEARCH_RESPONSE_TEXT =
  '{ results: { registrationNumber: string; name: string; city?: string; }[]; messageKey?: string; }';

describe('carrick#1749: the body a caller reads, stated by the source', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1749-'));
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
  async function infer(alias: string, expressionText: string) {
    const line = lineOf(expressionText);
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
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, `the row must be answered: ${alias}`);
    return inferred;
  }

  describe('(a) a cast of the body read is reported as the source statement', () => {
    it('reports a wrapper the source states, rooted at no name', async () => {
      const inferred = await infer('Members_Response', 'fetchReply("/v1/members")');
      assert.strictEqual(
        collapse(inferred.type_string),
        '{ members: { id: string; name: string; email: string; }[]; }'
      );
      assert.strictEqual(inferred.is_explicit, true);
      assert.deepStrictEqual(
        inferred.stated_body,
        {},
        'an object literal type is rooted at no name, so a model symbol naming the element cannot agree with it'
      );
    });

    it('reports the named root of a stated body, with where it is declared', async () => {
      const inferred = await infer('Member_Response', 'fetchReply("/v1/member")');
      assert.strictEqual(inferred.stated_body?.root, 'Member');
      assert.strictEqual(inferred.stated_body?.root_source, servicePath);
      assert.strictEqual(inferred.stated_body?.array_depth, undefined);
    });

    it('peels arrays and null from an annotated declaration the read initializes', async () => {
      const inferred = await infer('MemberOrNone_Response', 'fetchReply("/v1/member-or-none")');
      assert.strictEqual(inferred.stated_body?.root, 'Member');
      assert.strictEqual(inferred.stated_body?.array_depth, 1);
    });

    it('does not report an annotation further out than the read', async () => {
      // The declared type of the function the read sits in describes the
      // function, not the body; the read itself states nothing.
      const inferred = await infer('Loose_Response', 'fetchReply("/v1/loose")');
      assert.strictEqual(inferred.stated_body, undefined);
    });
  });

  describe('(b) a callback in the chain that casts the unread body', () => {
    it('reads the cast, not the value the chain ends in', async () => {
      const inferred = await infer('Search_Response', 'request(`/v1/search?q=${query}`)');
      assert.ok(
        !inferred.type_string.includes('suggestions'),
        `the mapped return is what the caller computed, not the body, got: ${inferred.type_string}`
      );
      assert.strictEqual(collapse(inferred.type_string), SEARCH_RESPONSE_TEXT);
      assert.strictEqual(inferred.is_explicit, true);
      assert.strictEqual(inferred.stated_body?.root, 'SearchResponse');
    });

    it('states nothing when no callback casts the body itself to a type', async () => {
      // One cast is of another value, one is of the body to \`unknown\`: neither
      // says what the body is.
      const inferred = await infer(
        'SearchUncast_Response',
        'request(`/v1/search-uncast?q=${query}`)'
      );
      assert.strictEqual(inferred.stated_body, undefined);
    });

    it('states nothing when two callbacks cast an unread value', async () => {
      const inferred = await infer('SearchTwo_Response', 'request(`/v1/search-two?q=${query}`)');
      assert.strictEqual(inferred.stated_body, undefined);
      assert.ok(
        !/registrationNumber/.test(inferred.type_string),
        `two candidate reads must not pick one, got: ${inferred.type_string}`
      );
    });

    it('does not read a parameter typed any', async () => {
      const inferred = await infer('SearchAny_Response', 'request(`/v1/search-any?q=${query}`)');
      assert.strictEqual(inferred.stated_body, undefined);
    });
  });
});
