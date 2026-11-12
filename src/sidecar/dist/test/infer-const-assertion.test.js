/**
 * carrick#2590: a const assertion states no type.
 *
 * A table written as an array literal closed by `as const` holds calls in
 * its entries' functions. The type a read states was looked for on the
 * nearest `as` around it, and the nearest one was the const assertion around
 * the whole table: its type node prints as `const`, which was published as
 * the call's type. Nothing declares a type named `const`, so capture decayed
 * the alias to `unknown` and the side reached the index untyped.
 *
 * A const assertion narrows its operand's own type; it names none. The read
 * keeps the type the compiler infers for it, and a cast stated at the read
 * still wins.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const TOOLS_TS = `export interface Settings {
  installEnabled: boolean;
  evaluator: string;
}

declare function getJson<T>(url: string): Promise<T>;

export function createClient() {
  return {
    async getSettings(project: string) {
      const response = await fetch(\`/settings/\${project}\`);
      const raw = await response.json();
      return { installEnabled: Boolean(raw.installEnabled), evaluator: String(raw.evaluator) };
    },
  };
}

type Client = ReturnType<typeof createClient>;

export const tools = [
  {
    name: 'settings',
    read: (client: Client, project: string) => client.getSettings(project),
  },
  {
    name: 'configure',
    write: (project: string, update: { installEnabled: boolean }) =>
      fetch(\`/configure/\${project}\`, { method: 'PUT', body: JSON.stringify(update) }),
  },
  {
    name: 'cast',
    read: (project: string) => getJson<unknown>(\`/cast/\${project}\`) as Promise<Settings>,
  },
] as const;
`;
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick#2590: a const assertion around a table states no type for the reads in it', () => {
    let client;
    let repoDir;
    let toolsPath;
    before(async () => {
        repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2590-')));
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
    async function infer(expression, inferKind) {
        const start = TOOLS_TS.indexOf(expression);
        assert.ok(start >= 0, `source must contain ${expression}`);
        assert.strictEqual(TOOLS_TS.indexOf(expression, start + 1), -1, `${expression} must be unique`);
        const line = TOOLS_TS.slice(0, start).split('\n').length;
        const res = await client.send({
            action: 'infer',
            request_id: expression,
            requests: [
                {
                    file_path: toolsPath,
                    line_number: line,
                    span_start: start,
                    span_end: start + expression.length,
                    infer_kind: inferKind,
                    alias: expression,
                },
            ],
        });
        const inferred = (res.inferred_types ?? []).find((t) => t.alias === expression);
        assert.ok(inferred, `${expression} must be answered`);
        return inferred;
    }
    it("publishes a call's inferred result, not the assertion's `const`", async () => {
        const inferred = await infer('client.getSettings(project)', 'call_result');
        assert.notStrictEqual(collapse(inferred.type_string), 'const');
        assert.match(collapse(inferred.type_string), /installEnabled: boolean/);
        assert.match(collapse(inferred.type_string), /evaluator: string/);
    });
    it("publishes a request body's inferred type, not the assertion's `const`", async () => {
        const inferred = await infer('JSON.stringify(update)', 'request_body');
        assert.notStrictEqual(collapse(inferred.type_string), 'const');
        assert.strictEqual(collapse(inferred.type_string), '{ installEnabled: boolean; }');
    });
    it('keeps a cast stated at the read', async () => {
        const inferred = await infer('getJson<unknown>(`/cast/${project}`)', 'call_result');
        assert.strictEqual(collapse(inferred.type_string), '{ installEnabled: boolean; evaluator: string; }');
        assert.strictEqual(inferred.is_explicit, true);
    });
});
