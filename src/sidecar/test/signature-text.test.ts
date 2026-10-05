/**
 * The signature pass's text does not depend on what the process that printed
 * it met first (carrick#1993).
 *
 * The compiler prints a union's members in the order it created them, so two
 * processes that read a program's files in different orders print one type
 * two ways. The signature pass is answered by several processes, each over its
 * own share of the files, and what it stores must be the same whoever
 * answered. Three layers are pinned:
 *
 * - the rewrite (`stableSignatureText`): every union at every depth in the
 *   canonical order the rest of the sidecar uses, and every other byte of the
 *   print as the compiler wrote it;
 * - the inferrer: a program whose files meet the same members in opposite
 *   orders, asked in different orders and in different shares by separate
 *   projects, answers the signature kinds byte for byte the same, while the
 *   contract kind `function_param` keeps the compiler's own print;
 * - over stdio: `signature_param` is a kind the schema accepts.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project, ts } from 'ts-morph';
import { stableSignatureText } from '../src/signature-text.js';
import { canonicalizeUnionsInText } from '../src/type-text-canonicalizer.js';
import { TypeInferrer } from '../src/type-inferrer.js';
import type { InferKind, InferRequestItem } from '../src/types.js';
import { SidecarClient } from './helpers.js';

describe('stableSignatureText (#1993)', () => {
  it('orders a union at the top level', () => {
    assert.strictEqual(stableSignatureText('Dog | Cat'), 'Cat | Dog');
    assert.strictEqual(stableSignatureText('"open" | "closed" | "held"'), '"closed" | "held" | "open"');
    // One rule for every member, as everywhere else in the sidecar: the
    // compiler's own habit of printing `null` last is not kept.
    assert.strictEqual(stableSignatureText('string | null'), 'null | string');
  });

  it('orders unions at every depth, inside function and conditional types too', () => {
    const cases: Array<[string, string]> = [
      ['Promise<Dog | Cat>', 'Promise<Cat | Dog>'],
      ['{ name: string; pet: Dog | Cat; }', '{ name: string; pet: Cat | Dog; }'],
      ['(Dog | Cat)[]', '(Cat | Dog)[]'],
      ['[Dog | Cat, string]', '[Cat | Dog, string]'],
      ['(pet: Dog | Cat) => "b" | "a"', '(pet: Cat | Dog) => "a" | "b"'],
      ['((pet: Dog | Cat) => void) | null', '((pet: Cat | Dog) => void) | null'],
      ['T extends string ? "b" | "a" : Dog | Cat', 'T extends string ? "a" | "b" : Cat | Dog'],
      ['{ [K in "b" | "a"]: Dog | Cat; }', '{ [K in "a" | "b"]: Cat | Dog; }'],
      ['{ [x: string]: Dog | Cat; }', '{ [x: string]: Cat | Dog; }'],
      ['new (pet: Dog | Cat) => Shelter', 'new (pet: Cat | Dog) => Shelter'],
      ['Omit<Dog, "name" | "age">', 'Omit<Dog, "age" | "name">'],
      ['{ adopt(pet: Dog | Cat): "b" | "a"; }', '{ adopt(pet: Cat | Dog): "a" | "b"; }'],
      // A member's own key decides its place: the nested union is ordered
      // first, then the outer one by the members as they now read.
      ['{ b: Z | Y; } | { a: Z | Y; }', '{ a: Y | Z; } | { b: Y | Z; }'],
    ];
    for (const [printed, stable] of cases) {
      assert.strictEqual(stableSignatureText(printed), stable, printed);
    }
  });

  it('leaves every list whose order means something as the compiler printed it', () => {
    for (const printed of [
      'Dog & Cat',
      '[string, number]',
      'Map<string, number>',
      '(b: string, a: number) => void',
      '{ z: 1; a: 2; }',
      '<T, U>(t: T, u: U) => [U, T]',
      'Record<string, Dog>',
      '{ [P in K]: Dog; }',
      '{ (x: number): B; (x: string): A; }',
      '{ f(x: number): B; f(x: string): A; }',
    ]) {
      assert.strictEqual(stableSignatureText(printed), printed, printed);
    }
  });

  it('returns a print with nothing to order byte for byte', () => {
    for (const printed of [
      'string',
      'string | undefined',
      'Order | null',
      'Promise<import("/srv/app/src/types").Order[]>',
      '{ readonly at: Date; id: string; tags?: string[] | undefined; }',
      '"draft" | `order-${number}`',
      'typeof import("/srv/app/src/db")',
      '{ [x: string]: any; }',
      'keyof Dog',
      'Dog["name"]',
      'readonly string[]',
      '(value: unknown) => asserts value is string',
      '(value: unknown) => value is string',
      '{ get name(): string; set name(value: string); }',
      'abstract new () => object',
      'unique symbol',
      'E.A | E.B',
      '{ "content-type": string; 0: number; }',
      'Array<{ a: string; }>',
    ]) {
      assert.strictEqual(stableSignatureText(printed), printed, printed);
    }
  });

  it('is its own fixed point', () => {
    for (const printed of [
      'Promise<{ pet: Dog | Cat; } | null>',
      '(pet: Dog | Cat) => "b" | "a"',
      '{ b: Z | Y; } | { a: Z | Y; }',
    ]) {
      const once = stableSignatureText(printed);
      assert.strictEqual(stableSignatureText(once), once, printed);
    }
  });

  it('orders by the rule the rest of the sidecar uses', () => {
    // Where the text-level canonicaliser rewrites, the two agree.
    for (const printed of [
      'Enum<{ code: "b" | "a"; raw: string; }>',
      'string | false | true',
      '{ status: "b" | "a"; } | null',
      'null | number',
    ]) {
      assert.strictEqual(stableSignatureText(printed), canonicalizeUnionsInText(printed), printed);
    }
  });

  it('hands a print the parser does not read cleanly to the text-level rule', () => {
    for (const printed of ['Dog | Cat >', '{ a: Dog | Cat; ', 'Dog | Cat; type X = 1']) {
      assert.strictEqual(stableSignatureText(printed), canonicalizeUnionsInText(printed), printed);
    }
  });
});

/**
 * Two files that meet the same members in opposite orders: whichever the
 * checker reads first fixes how every union of them prints.
 */
const FILES: Record<string, string> = {
  'src/pets.ts': `export interface Cat { kind: 'cat'; lives: number }
export interface Dog { kind: 'dog'; good: boolean }
export interface Bird { kind: 'bird'; wings: 2 }
`,
  'src/a.ts': `import { Bird, Cat, Dog } from './pets';
export function petA(n: number) {
  return n > 1 ? ({ kind: 'cat', lives: 9 } as Cat) : n > 0 ? ({ kind: 'dog', good: true } as Dog) : ({ kind: 'bird', wings: 2 } as Bird);
}
export function statusA(n: number) {
  return n > 1 ? ('open' as const) : n > 0 ? ('closed' as const) : ('held' as const);
}
export function adoptA(pet = petA(1)) {
  return { pet, status: statusA(1), log: [pet] };
}
`,
  'src/b.ts': `import { Bird, Cat, Dog } from './pets';
export function petB(n: number) {
  return n > 1 ? ({ kind: 'bird', wings: 2 } as Bird) : n > 0 ? ({ kind: 'dog', good: true } as Dog) : ({ kind: 'cat', lives: 9 } as Cat);
}
export function statusB(n: number) {
  return n > 1 ? ('held' as const) : n > 0 ? ('closed' as const) : ('open' as const);
}
export function adoptB(pet = petB(1)) {
  return Promise.resolve({ pet, status: statusB(1) });
}
`,
};

/** Every function's return slot and parameter slots, as the signature pass asks them. */
function slotsOf(root: string, file: string, paramKind: InferKind): InferRequestItem[] {
  const text = FILES[file];
  const filePath = path.join(root, file);
  const slots: InferRequestItem[] = [];
  text.split('\n').forEach((line, index) => {
    const declared = /^export function (\w+)\((\w+)?/.exec(line);
    if (!declared) return;
    const [, name, param] = declared;
    slots.push({
      file_path: filePath,
      line_number: index + 1,
      infer_kind: 'signature_return',
      alias: `${name}:return`,
    });
    if (param) {
      slots.push({
        file_path: filePath,
        line_number: index + 1,
        infer_kind: paramKind,
        alias: `${name}:${param}`,
        param_name: param,
      });
    }
  });
  return slots;
}

describe('the signature kinds print the same whatever a process met first (#1993)', () => {
  let root: string;

  before(() => {
    root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-signature-order-')));
    fs.writeFileSync(
      path.join(root, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
        include: ['src/**/*.ts'],
      }),
    );
    fs.mkdirSync(path.join(root, 'src'));
    for (const [file, text] of Object.entries(FILES)) fs.writeFileSync(path.join(root, file), text);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  /** A fresh project, so a fresh checker: what a separate process is. */
  function answer(requests: InferRequestItem[]): Map<string, string> {
    const inferrer = new TypeInferrer({
      project: new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') }),
      repoRoot: root,
    });
    const result = inferrer.infer(requests);
    assert.deepStrictEqual(result.errors ?? [], [], 'every slot answered');
    return new Map((result.inferred_types ?? []).map((t) => [t.alias, t.type_string]));
  }

  it('answers every slot the same in either order and in any share of the files', () => {
    const a = slotsOf(root, 'src/a.ts', 'signature_param');
    const b = slotsOf(root, 'src/b.ts', 'signature_param');
    const aFirst = answer([...a, ...b]);
    const bFirst = answer([...b, ...a]);
    const reversed = answer([...a, ...b].reverse());
    const onlyA = answer(a);
    const onlyB = answer(b);

    assert.strictEqual(aFirst.size, a.length + b.length, 'every slot answered');
    for (const [alias, text] of aFirst) {
      assert.strictEqual(bFirst.get(alias), text, `${alias} in the other order`);
      assert.strictEqual(reversed.get(alias), text, `${alias} with the slots reversed`);
      assert.strictEqual((onlyA.get(alias) ?? onlyB.get(alias)), text, `${alias} in its own file's share`);
    }
    assert.strictEqual(aFirst.get('petA:return'), 'Bird | Cat | Dog');
    assert.strictEqual(aFirst.get('statusB:return'), '"closed" | "held" | "open"');
    assert.strictEqual(aFirst.get('adoptA:pet'), 'Bird | Cat | Dog');
    assert.strictEqual(
      aFirst.get('adoptB:return'),
      'Promise<{ pet: Bird | Cat | Dog; status: "closed" | "held" | "open"; }>',
    );
  });

  it('keeps the compiler print for function_param, the kind contract types are asked by', () => {
    const a = slotsOf(root, 'src/a.ts', 'function_param').filter((r) => r.infer_kind === 'function_param');
    const b = slotsOf(root, 'src/b.ts', 'function_param').filter((r) => r.infer_kind === 'function_param');
    const aFirst = answer([...a, ...b]);
    const bFirst = answer([...b, ...a]);
    // The fixture does flip the compiler: the contract kind shows it, because
    // nothing rewrites what it prints.
    assert.notStrictEqual(aFirst.get('adoptB:pet'), bFirst.get('adoptB:pet'));

    // And what it prints is the compiler's print, byte for byte.
    const project = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
    const inferrer = new TypeInferrer({ project, repoRoot: root });
    const asked = inferrer.infer([...a, ...b]);
    const fresh = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
    for (const request of [...a, ...b]) {
      const fn = fresh
        .getSourceFileOrThrow(request.file_path)
        .getFunctions()
        .find((f) => f.getStartLineNumber() === request.line_number)!;
      const param = fn.getParameterOrThrow(request.param_name!);
      const printed = param
        .getType()
        .getText(param, ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.InTypeAlias);
      const answered = asked.inferred_types!.find((t) => t.alias === request.alias)!;
      assert.strictEqual(answered.type_string, printed, request.alias);
    }
  });
});

describe('signature_param over stdio (#1993)', () => {
  let client: SidecarClient;
  let root: string;

  before(async () => {
    root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-signature-stdio-')));
    fs.writeFileSync(
      path.join(root, 'tsconfig.json'),
      JSON.stringify({ compilerOptions: { strict: true, types: [] }, include: ['src/**/*.ts'] }),
    );
    fs.mkdirSync(path.join(root, 'src'));
    for (const [file, text] of Object.entries(FILES)) fs.writeFileSync(path.join(root, file), text);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'sig-init', repo_root: root });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('is accepted on a line-only locator and answered in the stable order', async () => {
    const response = await client.send<{
      status: string;
      inferred_types?: Array<{ alias: string; type_string: string }>;
      errors?: string[];
    }>({
      action: 'infer',
      request_id: 'sig-1',
      requests: [...slotsOf(root, 'src/b.ts', 'signature_param'), ...slotsOf(root, 'src/a.ts', 'signature_param')],
    });
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    const byAlias = new Map((response.inferred_types ?? []).map((t) => [t.alias, t.type_string]));
    assert.strictEqual(byAlias.get('adoptA:pet'), 'Bird | Cat | Dog');
    assert.strictEqual(byAlias.get('adoptB:pet'), 'Bird | Cat | Dog');
  });
});
