/**
 * carrick#2818: the type capture reads a labelled call's result.
 *
 * The library store labels which part of a request library's result holds the
 * response body: the whole awaited value, a member of it, or the return of a
 * parse method. The scanner sends the label on the consumer's infer item and
 * its capture anchor, and both readers (the v1 inferrer and the v2 capture)
 * take the contract from the labelled part, never from the library's envelope.
 *
 * Everything here is invented: a package `fixture-results` declared in the
 * fixture repo, with the general shapes request libraries have (a reply
 * wrapper, a data/error union, a thenable with parse methods, a hook whose
 * state is a union, a mutation hook).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import type { CaptureStubResult } from '../src/capture/api.js';

const PACKAGE_DTS = `declare module 'fixture-results' {
  export interface Reply<T> {
    data: T;
    status: number;
    headers: Record<string, string>;
  }
  export const reply: {
    get<T = any, R = Reply<T>>(url: string): Promise<R>;
  };
  export const sdk: {
    request<TData, TError>(options: { url: string }): Promise<
      (
        | { data: TData; error: undefined }
        | { data: undefined; error: TError }
      ) & { meta: string }
    >;
  };
  export interface Pending<R> extends Promise<R> {
    json<J = unknown>(): Promise<J>;
    text(): Promise<string>;
    user(): Promise<{ id: string; name: string }>;
  }
  export const lite: {
    get(url: string): Pending<{ status: number }>;
  };
  export type LoadResult<D, E> =
    | { data: undefined; error: null; state: 'pending' }
    | { data: D; error: null; state: 'success' }
    | { data: undefined; error: E; state: 'error' };
  export function useLoad<D = unknown, E = Error>(options: {
    key: string;
    load: () => Promise<D>;
  }): LoadResult<D, E>;
  export function useSave<D, E, V>(options: {
    save: (variables: V) => Promise<D>;
  }): { data: D | undefined; error: E | null; mutate(variables: V): void };
}
`;

const CONSUMER_TS = `import { reply, sdk, lite, useLoad, useSave } from 'fixture-results';

export interface User {
  id: string;
  name: string;
}
export interface ApiError {
  code: number;
}

declare function loadUser(): Promise<User>;
declare function saveUser(name: string): Promise<User>;

export async function wrapper() {
  return await reply.get<User>('/users/1');
}
export async function union() {
  return await sdk.request<User, ApiError>({ url: '/users/1' });
}
export async function parsed() {
  return await lite.get('/users/1');
}
export function hook() {
  return useLoad({ key: 'user', load: loadUser });
}
export function mutation() {
  return useSave({ save: saveUser });
}
`;

/** One labelled site: where it is, and the label the store gave it. */
interface Site {
  alias: string;
  text: string;
  result?: unknown;
}

const SITES: Site[] = [
  { alias: 'Wrapper', text: "reply.get<User>('/users/1')", result: { member: ['data'] } },
  { alias: 'Union', text: "sdk.request<User, ApiError>({ url: '/users/1' })", result: { member: ['data'] } },
  { alias: 'ParseConcrete', text: "lite.get('/users/1')", result: { parse: 'user' } },
  { alias: 'ParseGeneric', text: "lite.get('/users/1')", result: { parse: 'json' } },
  { alias: 'ParseText', text: "lite.get('/users/1')", result: { parse: 'text' } },
  { alias: 'Hook', text: "useLoad({ key: 'user', load: loadUser })", result: { member: ['data'] } },
  { alias: 'Mutation', text: 'useSave({ save: saveUser })', result: { member: ['data'] } },
  { alias: 'Whole', text: "reply.get<User>('/users/1')", result: { whole: true } },
  { alias: 'Missing', text: "reply.get<User>('/users/1')", result: { member: ['items'] } },
  { alias: 'MissingParse', text: "lite.get('/users/1')", result: { parse: 'blob' } },
  { alias: 'Unlabelled', text: "reply.get<User>('/users/1')" },
];

function spanOf(text: string): { start: number; end: number; line: number } {
  const start = CONSUMER_TS.indexOf(text);
  assert.ok(start >= 0, `fixture text not found: ${text}`);
  const line = CONSUMER_TS.slice(0, start).split('\n').length;
  return { start, end: start + text.length, line };
}

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ path: string; reason: string }>;
}
interface CaptureFrame {
  status: string;
  result?: CaptureStubResult;
  errors?: string[];
}

describe('carrick#2818: a labelled call result is read through its label', () => {
  let repoDir: string;
  let outRoot: string;
  const client = new SidecarClient();
  const inferred = new Map<string, Inferred>();
  let surface: string;
  let result: CaptureStubResult;

  before(async () => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2818-repo-')));
    outRoot = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2818-stub-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          rootDir: 'src',
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'fixture-results.d.ts'), PACKAGE_DTS);
    fs.writeFileSync(path.join(repoDir, 'src', 'consumer.ts'), CONSUMER_TS);

    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });

    const file = path.join(repoDir, 'src', 'consumer.ts');
    const res = await client.send<{ inferred_types?: Inferred[] }>({
      action: 'infer',
      request_id: 'infer-2818',
      requests: SITES.map((site) => {
        const span = spanOf(site.text);
        return {
          file_path: file,
          line_number: span.line,
          span_start: span.start,
          span_end: span.end,
          infer_kind: 'call_result',
          alias: site.alias,
          ...(site.result ? { result: site.result } : {}),
        };
      }),
    });
    for (const entry of res.inferred_types ?? []) inferred.set(entry.alias, entry);

    const frame = await client.send<CaptureFrame>(
      {
        action: 'capture_v2',
        request_id: 'capture-2818',
        repo_root: repoDir,
        service_name: 'consumer-svc',
        anchors: SITES.map((site) => {
          const span = spanOf(site.text);
          return {
            kind: 'infer',
            alias: `${site.alias}_Consumer_Response`,
            source_file: 'src/consumer.ts',
            anchor_origin: 'deterministic-infer',
            span_start: span.start,
            span_end: span.end,
            line_number: span.line,
            expression_text: site.text,
            ...(site.result ? { result: site.result } : {}),
          };
        }),
        out_dir: path.join(outRoot, 'stub'),
        scan_root: repoDir,
      },
      120_000
    );
    assert.strictEqual(frame.status, 'success', JSON.stringify(frame.errors));
    assert.ok(frame.result, 'the capture answered with no result');
    result = frame.result;
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  const v1 = (alias: string): Inferred => {
    const found = inferred.get(alias);
    assert.ok(found, `no v1 answer for ${alias}`);
    return found;
  };
  const v2 = (alias: string): string => {
    const match = surface.match(new RegExp(`export type ${alias}_Consumer_Response = ([^;]+);`));
    assert.ok(match, `no surface line for ${alias} in:\n${surface}`);
    return match[1].trim();
  };
  const abstainOf = (alias: string): string => {
    const record = result.aliases.find((entry) => entry.alias === `${alias}_Consumer_Response`);
    assert.ok(record, `no record for ${alias}`);
    return record.self_check_detail ?? '';
  };

  for (const alias of ['Wrapper', 'Union', 'Hook', 'Mutation']) {
    it(`${alias}: the member's type is the response, not the library's envelope`, () => {
      assert.match(v1(alias).type_string, /\bid: string\b/, v1(alias).type_string);
      assert.doesNotMatch(v1(alias).type_string, /status|headers|meta|state/);
      assert.match(v2(alias), /User|\bid: string\b/);
      assert.doesNotMatch(v2(alias), /status|headers|meta|state|undefined/);
    });
  }

  it('a parse method gives the awaited return of that method, read off the thenable', () => {
    assert.match(v1('ParseConcrete').type_string, /\bid: string\b/);
    assert.match(v2('ParseConcrete'), /\bid: string\b/);
    // `json<J>()` is generic and nothing at the call instantiates `J`: the
    // return is the method's own type parameter, as the ticket reads it.
    assert.strictEqual(v1('ParseGeneric').type_string, 'J');
    assert.strictEqual(v2('ParseGeneric'), 'unknown');
    assert.strictEqual(v1('ParseText').type_string, 'string');
    assert.strictEqual(v2('ParseText'), 'string');
  });

  it('whole is the awaited type', () => {
    assert.match(v1('Whole').type_string, /\bstatus: number\b/);
    assert.match(v2('Whole'), /Reply</);
  });

  it('a member the type lacks abstains with result_member_missing', () => {
    const missing = v1('Missing');
    assert.strictEqual(missing.type_string, 'unknown');
    assert.deepStrictEqual(
      missing.any_provenance?.map((p) => [p.path, p.reason]),
      [['', 'result_member_missing']]
    );
    assert.strictEqual(v1('MissingParse').any_provenance?.[0]?.reason, 'result_member_missing');
    assert.strictEqual(v2('Missing'), 'unknown');
    assert.match(abstainOf('Missing'), /^result_member_missing/);
    assert.match(abstainOf('MissingParse'), /^result_member_missing/);
  });

  it('an item without a result is read as it was: the library envelope stands', () => {
    assert.match(v1('Unlabelled').type_string, /Reply</);
    assert.match(v2('Unlabelled'), /Reply</);
    assert.strictEqual(v1('Unlabelled').any_provenance, undefined);
  });

  it('the request schemas refuse a label with two keys', async () => {
    const span = spanOf(SITES[0].text);
    const res = await client.send<{ status?: string }>({
      action: 'infer',
      request_id: 'bad-label',
      requests: [
        {
          file_path: path.join(repoDir, 'src', 'consumer.ts'),
          line_number: span.line,
          span_start: span.start,
          span_end: span.end,
          infer_kind: 'call_result',
          result: { whole: true, member: ['data'] },
        },
      ],
    });
    assert.notStrictEqual(res.status, 'success');
  });
});
