/**
 * carrick#2111: the type the model named for a producer's response is checked
 * against the compiler's type of what the handler sends.
 *
 * The model names one bare symbol per row. Where it names the element of a
 * list the handler sends, a helper's input type where the helper's result is
 * sent, or a type the handler wraps before sending, the row published the
 * symbol and a consumer of the real body read incompatible.
 *
 * The `infer` action takes the model's symbols beside its requests
 * (`model_symbols`, joined by alias) and answers on each inferred type how the
 * symbol relates to the read (`model_symbol_relation`). The compiler decides, by assignability both ways. The
 * driver keeps, re-depths or drops the symbol on that answer.
 *
 * Every request here goes through the process over stdio, so a field the
 * request schema dropped would show as a missing answer.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const CONTRACTS_TS = `export interface ItemDto { id: string; note?: string }
export interface StrictItemDto { id: string; note: string }
export interface StatusInput { on?: boolean; label?: string }
export interface StatusDto { on: boolean; label: string }
export type EventName = 'created' | 'removed';
export type EventNames = EventName[];
export interface Loose { id: string; extra: any }
export const itemShape = { id: 'x' };
`;
// A barrel, as a package's entry is: the model names the entry, not the file
// that declares the type.
const INDEX_TS = `export * from './contracts';
`;
const ROUTES_TS = `import type { ItemDto, StrictItemDto, StatusInput, StatusDto, EventName, Loose } from './index';

interface Row { id: string; note: string; secret: string }
declare const rows: Row[];
declare const one: Row | undefined;
declare const ok: boolean;
declare function readStatus(): StatusInput;
declare function loadLoose(): Loose;
declare function loadAny(): any;

function toDto(row: Row): Omit<ItemDto, 'note'> {
  return { id: row.id };
}
function toStrict(row: Row): Omit<StrictItemDto, 'note'> {
  return { id: row.id };
}
function normalise(input: StatusInput): StatusDto {
  return { on: input.on ?? false, label: input.label ?? '' };
}
const EVENTS = ['created', 'removed'] as const;
const NAMES: readonly EventName[] = EVENTS;

export function listOfSymbol() {
  return rows.map(toDto);
}
export function listOfAnother() {
  return rows.map(toStrict);
}
export function wrapped() {
  const item: ItemDto = { id: rows[0].id };
  return { item };
}
export function helperResult() {
  return normalise(readStatus());
}
export function orNull() {
  return one ? toDto(one) : null;
}
export function orError() {
  const item: ItemDto = { id: rows[0].id };
  return ok ? item : { error: 'x' };
}
export function declared(): ItemDto {
  return { id: rows[0].id };
}
export function constList() {
  return EVENTS;
}
export function readonlyList() {
  return NAMES;
}
export function holdsAny() {
  return loadLoose();
}
export function readsAny() {
  return loadAny();
}
declare const app: { get(path: string, handler: () => unknown): void };
app.get(
  '/wrapped-registration', // registration
  () => {
    const item: ItemDto = { id: rows[0].id };
    return { item };
  }
);
export class ItemsController {
  findOne(id: string): ItemDto | undefined { // method
    return rows.find((row) => row.id === id); // predicate
  }
}
`;
describe('carrick#2111: the model symbol of a producer response is related to the read', () => {
    let client;
    let repoDir;
    let routesPath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2111-'));
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                lib: ['es2022'],
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'src', 'contracts.ts'), CONTRACTS_TS);
        fs.writeFileSync(path.join(repoDir, 'src', 'index.ts'), INDEX_TS);
        routesPath = path.join(repoDir, 'src', 'routes.ts');
        fs.writeFileSync(routesPath, ROUTES_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    const lineOf = (marker) => {
        const index = ROUTES_TS.split('\n').findIndex((line) => line.includes(marker));
        assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
        return index + 1;
    };
    /** The relation answered for one handler's return against one named symbol. */
    const relate = async (handler, symbol, kind = 'function_return', sourceFile = 'src/index.ts') => {
        const alias = `Alias_${handler}_${symbol}_${kind}`;
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: routesPath,
                    line_number: lineOf(`export function ${handler}(`),
                    infer_kind: kind,
                    alias,
                    ...(kind === 'call_result' ? { expression_text: 'rows.map(toDto)' } : {}),
                },
            ],
            model_symbols: [{ alias, symbol_name: symbol, source_file: sourceFile }],
        });
        const inferred = response.inferred_types?.find((t) => t.alias === alias);
        return { relation: inferred?.model_symbol_relation, answered: inferred !== undefined };
    };
    const expectRelation = async (handler, symbol, relation, depth) => {
        const answer = await relate(handler, symbol);
        assert.ok(answer.answered, 'the row itself is still answered');
        assert.strictEqual(answer.relation?.relation, relation);
        assert.strictEqual(answer.relation?.depth, depth);
    };
    it('reads a list of the symbol, where the element and the symbol are one type', async () => {
        await expectRelation('listOfSymbol', 'ItemDto', 'list', 1);
    });
    it('reads a list of another type as different', async () => {
        await expectRelation('listOfAnother', 'StrictItemDto', 'different');
    });
    it('reads a wrapper around the symbol as different', async () => {
        await expectRelation('wrapped', 'ItemDto', 'different');
    });
    it("reads a helper's result as different from the helper's input type", async () => {
        await expectRelation('helperResult', 'StatusInput', 'different');
    });
    it('reads the symbol or null as nullable', async () => {
        await expectRelation('orNull', 'ItemDto', 'nullable');
    });
    it('reads the symbol beside another body as one member', async () => {
        await expectRelation('orError', 'ItemDto', 'member');
    });
    it('reads a declared return type equal to the symbol as the same', async () => {
        await expectRelation('declared', 'ItemDto', 'same');
    });
    it("reads a const list and a readonly list of the symbol as lists, the list's readonly set aside", async () => {
        await expectRelation('constList', 'EventName', 'list', 1);
        await expectRelation('readonlyList', 'EventName', 'list', 1);
    });
    it('reads a readonly list as the same as a symbol that names the mutable list', async () => {
        // Published as the read, a readonly list against a consumer's mutable one
        // is refused for the modifier alone, which JSON does not carry.
        await expectRelation('readonlyList', 'EventNames', 'same');
    });
    // Nothing to compare: no answer, and the driver keeps the symbol.
    it('answers nothing where the read holds any, at its root or inside', async () => {
        assert.strictEqual((await relate('holdsAny', 'Loose')).relation, undefined);
        assert.strictEqual((await relate('readsAny', 'ItemDto')).relation, undefined);
    });
    it('answers nothing where the symbol does not resolve to a type', async () => {
        assert.strictEqual((await relate('declared', 'NoSuchType')).relation, undefined);
        assert.strictEqual((await relate('declared', 'itemShape')).relation, undefined);
        assert.strictEqual((await relate('declared', 'ItemDto', 'function_return', 'src/missing.ts')).relation, undefined);
    });
    it('answers nothing for a consumer call, whose text can be a later expression', async () => {
        assert.strictEqual((await relate('listOfSymbol', 'ItemDto', 'call_result')).relation, undefined);
    });
    // A locator that binds inside a callback of the handler reads the callback,
    // not the route: `row` here is answered with the predicate's `boolean`. A
    // read of the wrong function must never displace the symbol the model
    // named, so the relation is asked only of the function the row's line names.
    it('answers nothing where the locator binds inside a callback of the handler', async () => {
        for (const kind of ['function_return', 'response_body']) {
            const alias = `Alias_findOne_${kind}`;
            const response = await client.send({
                action: 'infer',
                request_id: `infer-${alias}`,
                requests: [
                    {
                        file_path: routesPath,
                        line_number: lineOf('// method'),
                        expression_text: 'row',
                        expression_line: lineOf('// predicate'),
                        infer_kind: kind,
                        alias,
                    },
                ],
                model_symbols: [{ alias, symbol_name: 'ItemDto', source_file: 'src/index.ts' }],
            });
            const inferred = response.inferred_types?.find((t) => t.alias === alias);
            assert.strictEqual(inferred?.model_symbol_relation, undefined, `${kind}: ${inferred?.type_string}`);
        }
        // The same method read as itself relates as the source states it.
        const alias = 'Alias_findOne_itself';
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                { file_path: routesPath, line_number: lineOf('// method'), infer_kind: 'function_return', alias },
            ],
            model_symbols: [{ alias, symbol_name: 'ItemDto', source_file: 'src/index.ts' }],
        });
        assert.strictEqual(response.inferred_types?.[0]?.model_symbol_relation?.relation, 'nullable');
    });
    it('relates the handler of a registration that opens on the row line, wherever the handler opens', async () => {
        const alias = 'Alias_registration';
        const response = await client.send({
            action: 'infer',
            request_id: `infer-${alias}`,
            requests: [
                {
                    file_path: routesPath,
                    line_number: lineOf('// registration') - 1,
                    infer_kind: 'function_return',
                    alias,
                },
            ],
            model_symbols: [{ alias, symbol_name: 'ItemDto', source_file: 'src/index.ts' }],
        });
        assert.strictEqual(response.inferred_types?.[0]?.model_symbol_relation?.relation, 'different');
    });
    it('answers nothing where no symbol was sent for the alias', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'infer-no-symbol',
            requests: [
                {
                    file_path: routesPath,
                    line_number: lineOf('export function listOfSymbol('),
                    infer_kind: 'function_return',
                    alias: 'NoSymbol',
                },
            ],
        });
        assert.strictEqual(response.inferred_types?.[0]?.model_symbol_relation, undefined);
    });
});
