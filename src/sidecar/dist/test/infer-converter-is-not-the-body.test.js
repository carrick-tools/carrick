/**
 * carrick#2595: a consumer's untyped response handed to a function that
 * builds a new value from it was published as that function's return type.
 * The return type describes what the function built, not the body the call
 * answered with, so the row was concretely wrong where `unknown` was honest.
 *
 * Two readings took such a call as the body:
 *
 *  - a call around the body read whose result is bound to an object
 *    (`const pull = toPull(await response.json())`, carrick#1382);
 *  - the def-use walk over a call whose own value is untyped, which took any
 *    later declaration that used the value whole
 *    (`const raw = await readRaw(); const pull = toPull(raw)`, carrick#1375).
 *
 * Both now take a call only where it states its argument: a schema validates
 * it, or the callee returns it unchanged (through casts) or an empty default.
 * A function declared without a body (`declare`, a `.d.ts`) cannot be read,
 * and a parser and a converter share the signature `(value: unknown) => T`,
 * so its return type is not taken either. No library, method or framework
 * name is matched.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const SERVICE_TS = `export interface Schema<Output, Input = Output> {
  parse(data: unknown): Output;
  readonly _output: Output;
  readonly _input: Input;
}
declare function schemaOf<T>(): Schema<T>;

// The platform's response: its json() is the body read by declaration (blueprint row N33).
type TransportBody = Response;
declare function request(path: string): Promise<TransportBody>;
declare function readRaw(path: string): Promise<unknown>;

export type Pull = { messages: { leaseId: string }[]; backlog: number };
export type Counted = { count: number };

function toCounted(value: unknown): Counted {
  return { count: Array.isArray(value) ? value.length : 0 };
}

function asPull(value: unknown): Pull {
  if (!value) return {} as Pull;
  return value as Pull;
}

declare function fromDeclaration(value: unknown): Pull;

const PullSchema = schemaOf<Pull>();

export async function readConverted() {
  const response = await request('/v1/read/converted');
  const counted = toCounted(await response.json());
  return counted;
}

export async function readNarrowed() {
  const response = await request('/v1/read/narrowed');
  const pull = asPull(await response.json());
  return pull;
}

export async function readValidated() {
  const response = await request('/v1/read/validated');
  const pull = PullSchema.parse(await response.json());
  return pull;
}

export async function readDeclared() {
  const response = await request('/v1/read/declared');
  const pull = fromDeclaration(await response.json());
  return pull;
}

export async function laterDeclared() {
  const raw = await readRaw('/v1/later/declared');
  const pull = fromDeclaration(raw);
  console.log(pull);
}

export async function laterConverted() {
  const raw = await readRaw('/v1/later/converted');
  const counted = toCounted(raw);
  console.log(counted);
}

export async function laterNarrowed() {
  const raw = await readRaw('/v1/later/narrowed');
  const pull = asPull(raw);
  console.log(pull);
}

export async function laterValidated() {
  const raw = await readRaw('/v1/later/validated');
  const pull = PullSchema.parse(raw);
  console.log(pull);
}
`;
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick#2595: a converter is not the body it was handed', () => {
    let client;
    let repoDir;
    let sourcePath;
    const lineOf = (marker) => {
        const index = SERVICE_TS.split('\n').findIndex((line) => line.includes(marker));
        assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
        return index + 1;
    };
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2595-'));
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
        sourcePath = path.join(repoDir, 'src', 'service.ts');
        fs.writeFileSync(sourcePath, SERVICE_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init-2595', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    const callResult = async (alias, expression, at) => {
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: sourcePath,
                    line_number: lineOf(at),
                    expression_text: expression,
                    expression_line: lineOf(at),
                    infer_kind: 'call_result',
                    alias,
                },
            ],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        return inferred ? collapse(inferred.type_string) : undefined;
    };
    const assertPull = (text) => {
        assert.ok(text, 'the row must be answered');
        assert.match(text, /leaseId: string/, `expected the Pull shape, got ${text}`);
        assert.match(text, /backlog: number/, `expected the Pull shape, got ${text}`);
    };
    // carrick#1382: a call around the body read.
    it('leaves a body read handed to a converter untyped', async () => {
        const text = await callResult('ReadConverted', "request('/v1/read/converted')", "request('/v1/read/converted')");
        assert.strictEqual(text, 'any');
    });
    it('leaves a body read handed to a function declared without a body untyped', async () => {
        const text = await callResult('ReadDeclared', "request('/v1/read/declared')", "request('/v1/read/declared')");
        assert.strictEqual(text, 'any');
    });
    it('still reads a body read handed to a function that returns it unchanged', async () => {
        assertPull(await callResult('ReadNarrowed', "request('/v1/read/narrowed')", "request('/v1/read/narrowed')"));
    });
    it('still reads a body read handed to a schema', async () => {
        assertPull(await callResult('ReadValidated', "request('/v1/read/validated')", "request('/v1/read/validated')"));
    });
    // carrick#1375: the def-use walk over an untyped call value.
    it('leaves an untyped call value handed later to a converter untyped', async () => {
        const text = await callResult('LaterConverted', "readRaw('/v1/later/converted')", "readRaw('/v1/later/converted')");
        assert.strictEqual(text, 'unknown');
    });
    it('leaves an untyped call value handed later to a function declared without a body untyped', async () => {
        const text = await callResult('LaterDeclared', "readRaw('/v1/later/declared')", "readRaw('/v1/later/declared')");
        assert.strictEqual(text, 'unknown');
    });
    it('still follows an untyped call value into a function that returns it unchanged', async () => {
        assertPull(await callResult('LaterNarrowed', "readRaw('/v1/later/narrowed')", "readRaw('/v1/later/narrowed')"));
    });
    it('still follows an untyped call value into a schema', async () => {
        assertPull(await callResult('LaterValidated', "readRaw('/v1/later/validated')", "readRaw('/v1/later/validated')"));
    });
});
