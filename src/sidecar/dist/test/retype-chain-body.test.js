/**
 * Phase 3 step 3 (carrick#2472, P3-4): a consumer reads the body of a `fetch`
 * Response where it is bound by a promise chain or by a `Promise.all`
 * destructuring, not by a `const` beside the call.
 *
 *   fetch(u).then((r) => r.json()).then((d) => use(d.x))
 *   const [a, b] = await Promise.all([fetch(u1), fetch(u2)])
 *
 * The body read is `r.json()` on the callback's first parameter, or `a.json()`
 * on the element the call sits at. The retype casts that read to the
 * producer's type, and the success-path rules of carrick#1505 apply unchanged.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
// A `// read` mark ends the line a field read is flagged on. Each function's
// located call is the first `fetch(` (or the one named) after its declaration.
const CONSUMER = `export interface Order {
  x: number;
}

declare function keep(value: unknown): void;
declare const Pool: { all<T>(items: T[]): Promise<T[]> };

export function thenChain(): Promise<number> {
  return fetch('/p')
    .then((r) => r.json())
    .then((d) => d.x); // read
}

export function thenBlock(): Promise<number> {
  return fetch('/p')
    .then((r) => {
      return r.json();
    })
    .then((d) => d.x); // read
}

export function thenFunction(): Promise<number> {
  return fetch('/p')
    .then(function (r) {
      return r.json();
    })
    .then((d) => d.x); // read
}

export function thenOkGuard(): Promise<number> {
  return fetch('/p')
    .then((r) => {
      if (!r.ok) throw new Error('failed');
      return r.json();
    })
    .then((d) => d.x); // read
}

export function thenErrorOnly(): Promise<number> {
  return fetch('/p')
    .then((r) => {
      if (!r.ok) {
        return r.json();
      }
      return null;
    })
    .then((d) => (d ? d.x : 0));
}

export function thenHandedOn(): Promise<number> {
  return fetch('/p')
    .then((r) => {
      keep(r);
      return r.json();
    })
    .then((d) => d.x);
}

export function thenNoRead(): Promise<number> {
  return fetch('/p').then((r) => r.status);
}

export function thenReturned() {
  return fetch('/p').then((r) => r.json());
}

export function thenDestructured(): Promise<number> {
  return fetch('/p')
    .then(({ json }) => json())
    .then((d) => d.x);
}

export async function allDestructured(): Promise<number> {
  const [a, b] = await Promise.all([fetch('/p'), fetch('/q')]);
  const first = await a.json();
  const second = await b.json();
  return first.x + second.x;
}

export async function allEach(): Promise<number> {
  const [a, b] = await Promise.all([fetch('/p'), fetch('/q')]);
  const first = await a.json();
  const second = await b.json();
  const one = first.x; // read-a
  const two = second.x; // read-b
  return one + two;
}

export async function allOkGuard(): Promise<number> {
  const [a] = await Promise.all([fetch('/p')]);
  if (!a.ok) throw new Error('failed');
  const body = await a.json();
  return body.x; // read
}

export async function allHandedOn(): Promise<number> {
  const [a] = await Promise.all([fetch('/p')]);
  keep(a);
  const body = await a.json();
  return body.x;
}

export async function allAfterSpread(more: Promise<Response>[]): Promise<number> {
  const [a, b] = await Promise.all([...more, fetch('/p')]);
  const body = await b.json();
  return body.x;
}

export async function allLookalike(): Promise<number> {
  const [a] = await Pool.all([fetch('/p')]);
  const body = await a.json();
  return body.x;
}

export async function allUnbound(): Promise<number> {
  const all = await Promise.all([fetch('/p')]);
  const body = await all[0].json();
  return body.x;
}
`;
const lines = CONSUMER.split('\n');
/** The 1-based line of the first `text` after `export ... function name`. */
function locate(name, text) {
    const start = lines.findIndex((l) => new RegExp(`function ${name}\\b`).test(l));
    assert.ok(start >= 0, name);
    const at = lines.findIndex((l, i) => i >= start && l.includes(text));
    assert.ok(at >= 0, `${name}: ${text}`);
    return at + 1;
}
/** The 1-based line of the `// read` mark in the function. */
function readLine(name, mark = '// read') {
    const start = lines.findIndex((l) => new RegExp(`function ${name}\\b`).test(l));
    const at = lines.findIndex((l, i) => i >= start && l.includes(mark));
    return at + 1;
}
describe('carrick#2472 P3-4: retype a body read bound by a chain or a Promise.all', () => {
    let client;
    let repoDir;
    let consumerPath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-p34-'));
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
        consumerPath = path.join(repoDir, 'src', 'orders.ts');
        fs.writeFileSync(consumerPath, CONSUMER);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    async function retype(name, producerType, text = "fetch('/p')") {
        const line = locate(name, text);
        const res = await client.send({
            action: 'retype_check',
            request_id: `${name}-${text}-${producerType}`,
            items: [
                {
                    item_id: name,
                    file_path: consumerPath,
                    line_number: line,
                    expression_text: text,
                    expression_line: line,
                    producer_type: producerType,
                    wire: true,
                },
            ],
        });
        assert.strictEqual(res.status, 'success', JSON.stringify(res));
        return res.outcomes[0];
    }
    for (const name of ['thenChain', 'thenBlock', 'thenFunction', 'thenOkGuard']) {
        it(`(${name}) flags the read of a field the producer does not return`, async () => {
            const out = await retype(name, '{ y: number; }');
            assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
            assert.strictEqual(out.diagnostics.length, 1, JSON.stringify(out.diagnostics));
            assert.strictEqual(out.diagnostics[0].line, readLine(name));
            assert.strictEqual(out.diagnostics[0].code, 2339);
        });
        it(`(${name}) flags nothing when the producer returns the field`, async () => {
            const out = await retype(name, '{ x: number; }');
            assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
        });
    }
    for (const name of ['thenErrorOnly', 'thenHandedOn', 'thenNoRead', 'thenReturned', 'thenDestructured']) {
        it(`(${name}) abstains`, async () => {
            const out = await retype(name, '{ y: number; }');
            assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
        });
    }
    it('(thenErrorOnly) says the body is read only on the error path', async () => {
        const out = await retype('thenErrorOnly', '{ y: number; }');
        assert.match(out.reason ?? '', /error path/);
    });
    it('(thenReturned) says the readers are elsewhere', async () => {
        const out = await retype('thenReturned', '{ y: number; }');
        assert.match(out.reason ?? '', /readers are elsewhere/);
    });
    it('(allEach) reads each element at its own binding', async () => {
        const first = await retype('allEach', '{ y: number; }', "fetch('/p')");
        assert.strictEqual(first.outcome, 'mismatch', JSON.stringify(first));
        assert.deepStrictEqual(first.diagnostics.map((d) => d.line), [readLine('allEach', '// read-a')]);
        const second = await retype('allEach', '{ y: number; }', "fetch('/q')");
        assert.strictEqual(second.outcome, 'mismatch', JSON.stringify(second));
        assert.deepStrictEqual(second.diagnostics.map((d) => d.line), [readLine('allEach', '// read-b')]);
    });
    it('(allDestructured) agrees when the producer returns the field', async () => {
        const out = await retype('allDestructured', '{ x: number; }', "fetch('/q')");
        assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
    });
    it('(allOkGuard) flags the success-path read', async () => {
        const out = await retype('allOkGuard', '{ y: number; }');
        assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
        assert.strictEqual(out.diagnostics[0].line, readLine('allOkGuard'));
        const agreeing = await retype('allOkGuard', '{ x: number; }');
        assert.strictEqual(agreeing.outcome, 'agrees', JSON.stringify(agreeing));
    });
    for (const name of ['allHandedOn', 'allAfterSpread', 'allLookalike', 'allUnbound']) {
        it(`(${name}) abstains`, async () => {
            const out = await retype(name, '{ y: number; }');
            assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
        });
    }
});
