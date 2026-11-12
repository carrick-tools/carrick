/**
 * An annotation on a const does not describe a call inside a function in its
 * initializer.
 *
 * `const x: T = { handler: () => client.op(arg) }` annotates the object. The
 * call sits inside an arrow, so `T` is not the call's value: the call keeps
 * the type the compiler infers for it and is not explicit. A direct
 * `const y: T = client.op(arg)` still reads `T`.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const SRC = `export interface Handlers {
  handler: () => unknown;
}

export interface Wanted {
  ok: boolean;
  echoed: string;
  stated: string;
}

export function createClient() {
  return {
    async op(arg: string) {
      return { ok: true, echoed: arg };
    },
  };
}

const client = createClient();

export const nested: Handlers = {
  handler: () => client.op('nested'),
};

export async function direct() {
  const y: Promise<Wanted> = client.op('direct');
  return y;
}
`;
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('an annotated const does not type a call inside a function in its initializer', () => {
    let client;
    let repoDir;
    let filePath;
    before(async () => {
        repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-explicit-boundary-')));
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
        filePath = path.join(repoDir, 'src', 'handlers.ts');
        fs.writeFileSync(filePath, SRC);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    async function infer(expression) {
        const start = SRC.indexOf(expression);
        assert.ok(start >= 0, `source must contain ${expression}`);
        const line = SRC.slice(0, start).split('\n').length;
        const res = await client.send({
            action: 'infer',
            request_id: expression,
            requests: [
                {
                    file_path: filePath,
                    line_number: line,
                    span_start: start,
                    span_end: start + expression.length,
                    infer_kind: 'call_result',
                    alias: expression,
                },
            ],
        });
        const inferred = (res.inferred_types ?? []).find((t) => t.alias === expression);
        assert.ok(inferred, `${expression} must be answered`);
        return inferred;
    }
    it("keeps a call inside an arrow in an annotated const's own type, not explicit", async () => {
        const inferred = await infer("client.op('nested')");
        assert.strictEqual(inferred.is_explicit, false);
        assert.doesNotMatch(collapse(inferred.type_string), /Handlers|=>/);
        assert.match(collapse(inferred.type_string), /ok: boolean/);
    });
    it('still reads the annotation of a direct annotated const', async () => {
        const inferred = await infer("client.op('direct')");
        assert.strictEqual(collapse(inferred.type_string), 'Wanted');
    });
});
