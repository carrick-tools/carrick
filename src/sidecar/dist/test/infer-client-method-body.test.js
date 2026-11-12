/**
 * carrick#2628: a call into a client method that builds the request inside
 * itself.
 *
 * A table of tools (closed by `as const`) holds handlers that call methods
 * of an in-repo client: `client.addTags(project, unique(tags))`. The method
 * wraps its argument before it sends it, `body: { tags }`. The consumer row
 * sits at the handler's call, so the request locator names the call's
 * argument and the response locator names the call.
 *
 * - The argument is not the body: the method's parameter is not a body slot,
 *   and the method sends `{ tags: string[] }`, not `string[]`. The request is
 *   the body the method builds, or unresolved when it cannot be told.
 * - A handler that returns its own object built around the call's result
 *   (`return { ok: true, count: count(await client.add(...)) }`) returns the
 *   handler's value, not the HTTP response. The response is the call's own
 *   result.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const CLIENT_TS = `export interface TagDto {
  id: string;
  name: string;
}

interface SendOptions<TBody> {
  path: { name: string };
  body: TBody;
}

declare function postTags(options: SendOptions<{ tags: string[] }>): Promise<{ data?: TagDto[] }>;
declare function postNote(options: SendOptions<{ note: string }>): Promise<{ data?: TagDto }>;
declare function postEvent(options: SendOptions<Record<string, unknown>>): Promise<{ data?: TagDto }>;

export class Client {
  private async invoke<T>(call: () => Promise<{ data?: unknown }>): Promise<T> {
    const result = await call();
    return result.data as T;
  }

  async addTags(project: string, tags: string[]): Promise<TagDto[]> {
    return this.invoke<TagDto[]>(() => postTags({ path: { name: project }, body: { tags } }));
  }

  async addNote(project: string, note: { note: string }): Promise<TagDto> {
    return this.invoke<TagDto>(() => postNote({ path: { name: project }, body: note }));
  }

  async addLabels(project: string, labels: string[]): Promise<void> {
    await this.invoke<unknown>(() => this.send(project, [labels]));
  }

  async addEvent(project: string, event?: Record<string, unknown>): Promise<TagDto> {
    return this.invoke<TagDto>(() => postEvent({ path: { name: project }, body: (event ?? {}) as never }));
  }

  private async send(project: string, payload: unknown): Promise<{ data?: unknown }> {
    return { data: [project, payload] };
  }
}
`;
const TOOLS_TS = `import type { Client } from './client';

const unique = (values: string[]): string[] => [...new Set(values)];
const countOf = (rows: unknown[]): number => rows.length;

export const tools = [
  {
    name: 'tags_add',
    handler: async (client: Client, input: { project: string; tags: string[] }) => {
      await client.addTags(input.project, unique(input.tags));
    },
  },
  {
    name: 'tags_add_counted',
    handler: async (client: Client, input: { project: string; tags: string[] }) => {
      return { ok: true, count: countOf(await client.addTags(input.project, input.tags)) };
    },
  },
  {
    name: 'note_add',
    handler: (client: Client, input: { project: string; note: { note: string } }) =>
      client.addNote(input.project, input.note),
  },
  {
    name: 'labels_add',
    handler: async (client: Client, input: { project: string; labels: string[] }) => {
      await client.addLabels(input.project, input.labels);
    },
  },
  {
    name: 'event_add',
    handler: (client: Client, input: { project: string; event?: { kind: string } }) =>
      client.addEvent(input.project, input.event),
  },
] as const;
`;
/** An answer of `unknown` the inferrer decided, so capture keeps it rather than re-reading the argument. */
const DECIDED = 'unknown (body_built_by_callee)';
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick#2628: a call into a client method that builds the request inside itself', () => {
    let client;
    let repoDir;
    let toolsPath;
    before(async () => {
        repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2628-')));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                lib: ['es2022', 'dom'],
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        fs.writeFileSync(path.join(repoDir, 'src', 'client.ts'), CLIENT_TS);
        toolsPath = path.join(repoDir, 'src', 'tools.ts');
        fs.writeFileSync(toolsPath, TOOLS_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    /** The answer for `expression` located by its text on its line, as a model locator names it; undefined when unresolved. */
    async function infer(expression, inferKind, occurrence = 0) {
        let start = -1;
        for (let i = 0; i <= occurrence; i++)
            start = TOOLS_TS.indexOf(expression, start + 1);
        assert.ok(start >= 0, `source must contain ${expression} (#${occurrence})`);
        const line = TOOLS_TS.slice(0, start).split('\n').length;
        const alias = `${inferKind}:${expression}:${occurrence}`;
        const res = await client.send({
            action: 'infer',
            request_id: alias,
            requests: [
                {
                    file_path: toolsPath,
                    line_number: line,
                    expression_text: expression,
                    expression_line: line,
                    infer_kind: inferKind,
                    alias,
                },
            ],
        });
        const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
        if (!inferred)
            return undefined;
        const root = (inferred.any_provenance ?? []).find((p) => p.path === '');
        const text = collapse(inferred.type_string);
        return text === 'unknown' && root ? `${text} (${root.reason})` : text;
    }
    it('reads the body the method wraps its argument into, never the argument', async () => {
        const body = await infer('unique(input.tags)', 'request_body');
        assert.notStrictEqual(body, 'string[]');
        assert.strictEqual(body, '{ tags: string[]; }');
    });
    it('keeps the argument when the method sends it unchanged as the body', async () => {
        const body = await infer('input.note', 'request_body');
        assert.strictEqual(body, '{ note: string; }');
    });
    it('decides no request when the method wraps the argument into something it does not send as a body', async () => {
        const body = await infer('input.labels', 'request_body');
        assert.strictEqual(body, DECIDED);
    });
    it('decides no request when the body the method derives from its argument holds no type', async () => {
        const body = await infer('input.event', 'request_body');
        assert.strictEqual(body, DECIDED);
    });
    it("reads the call's own result, not the handler's object built around it", async () => {
        const response = await infer('client.addTags(', 'call_result', 1);
        assert.doesNotMatch(response ?? '', /\bok\b/);
        assert.match(response ?? '', /TagDto\[\]|\{ id: string; name: string; \}\[\]/);
    });
});
