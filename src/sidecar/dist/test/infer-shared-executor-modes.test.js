/**
 * carrick#2372: a route whose response is read from an executor it shares
 * with other routes, which tell it apart by an argument.
 *
 * `POST /x` calls `execute(input, 'commit')` and `POST /x/preview` calls
 * `execute(input, 'preview')`; a list route calls `read({ name })` and an
 * item route `read({ name, id })`. The executor's returns (or sends) are the
 * union of every caller's answers, and the compiler types every call of a
 * signature that is neither generic nor overloaded the same way. Each route
 * gets the bodies its own arguments leave reachable. Where the arguments
 * cannot tell the bodies apart, the union stays and the row lists the
 * argument as a read no case can be picked by (`response_modes`, location
 * `argument`), so a mismatch against it is not reported as the route's.
 *
 * Every name is a placeholder. The server library is invented; its reply's
 * `send` is a labelled body send.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const PKG = 'route-kit';
const LIBRARY_DTS = `export interface Reply {
  status(code: number): Reply;
  send(body?: unknown): Reply;
}
export interface Request {
  body: unknown;
  params: Record<string, string>;
}
export type Handler = (req: Request, reply: Reply) => unknown;
export interface App {
  get(path: string, handler: Handler): App;
  post(path: string, handler: Handler): App;
}
declare function make(): App;
export default make;
export declare const reply: Reply;
`;
const ROUTES_TS = `import make from '${PKG}';
import type { Reply, Request } from '${PKG}';

const app = make();

interface ListDto { items: string[] }
interface ItemDto { id: string; parts: number[] }
declare function transaction(work: () => void): void;

class Rollback extends Error {
  constructor(readonly preview: { changed: boolean; fields: string[] }) {
    super('rollback');
  }
}

// A mode flag; the executor returns its body.
async function execute(input: unknown, mode: 'commit' | 'preview') {
  void input;
  if (mode === 'preview') return { changed: true, fields: ['a'] };
  return { id: 'p1', name: 'n' };
}
app.post('/flag', (req) => execute(req.body, 'commit'));
app.post('/flag/preview', (req) => execute(req.body, 'preview'));

// A declared union return; an optional member of the argument picks it.
function read(input: { name: string; id?: string }): ListDto | ItemDto {
  const { id } = input;
  if (id === undefined) return { items: [input.name] };
  return { id, parts: [] };
}
app.get('/things', async (req) => {
  return read({ name: req.params.name });
});
app.get('/things/:id/parts', async (req) => {
  return read({ name: req.params.name, id: req.params.id });
});

// A mode flag; the executor sends its body through the reply.
const executeSend = async (req: Request, reply: Reply, mode: 'commit' | 'preview') => {
  void req;
  if (mode === 'preview') return reply.status(200).send({ changed: true, fields: ['a'] });
  return reply.status(200).send({ id: 'p1', name: 'n' });
};
app.post('/send', (req, reply) => executeSend(req, reply, 'commit'));
app.post('/send/preview', (req, reply) => executeSend(req, reply, 'preview'));

// A mode the executor decides through a throw: no test is on a send's path.
const executeThrow = async (req: Request, reply: Reply, mode: 'commit' | 'preview') => {
  void req;
  try {
    transaction(() => {
      if (mode === 'preview') throw new Rollback({ changed: true, fields: ['a'] });
    });
  } catch (error) {
    if (error instanceof Rollback) return reply.status(200).send(error.preview);
    throw error;
  }
  return reply.status(200).send({ id: 'p1', name: 'n' });
};
app.post('/throw', (req, reply) => executeThrow(req, reply, 'commit'));
app.post('/throw/preview', (req, reply) => executeThrow(req, reply, 'preview'));

// A declared union built in a local function: the argument decides it, out of reach.
function readBuilt(input: { name: string; id?: string }): ListDto | ItemDto {
  const { id } = input;
  const build = (): ListDto | ItemDto => (id === undefined ? { items: [input.name] } : { id, parts: [] });
  const response = build();
  return response;
}
app.get('/built', async (req) => {
  return readBuilt({ name: req.params.name });
});
app.get('/built/:id/parts', async (req) => {
  return readBuilt({ name: req.params.name, id: req.params.id });
});

// One caller only: the literal picks nothing among callers.
async function executeOnce(input: unknown, mode: 'commit' | 'preview') {
  void input;
  if (mode === 'preview') return { changed: true, fields: ['a'] };
  return { id: 'p1', name: 'n' };
}
app.post('/once', (req) => executeOnce(req.body, 'commit'));

// Every caller passes the same literal, and the executor answers a union
// the literal does not decide.
async function executeSame(input: unknown, mode: 'commit' | 'preview') {
  if (mode === 'preview') return { changed: true, fields: ['a'] };
  if (input) return { id: 'p1', name: 'n' };
  return { missing: true };
}
app.post('/same/a', (req) => executeSame(req.body, 'commit'));
app.post('/same/b', (req) => executeSame(req.body, 'commit'));
`;
let sequence = 0;
const claim = (exported, receiver, body) => ({
    claim_id: `c${sequence++}`,
    package: PKG,
    export: exported,
    role: 'server_framework',
    receiver,
    claim: body,
});
/** The labels as the store answers them: `send` is a body send. */
const LABELS = [
    claim('default', 'export', { kind: 'make', form: 'call', member: null }),
    ...['get', 'post'].map((member) => claim('default', 'instance:()', {
        kind: 'op',
        op: 'receive',
        member,
        of: 'instance:()',
        name: { arg: 0 },
        handler: { arg: 1 },
        method: member.toUpperCase(),
        request: { param: 0, body: ['body'] },
    })),
    claim('reply', 'export', { kind: 'op', op: 'send', member: 'send', on: 'export', payload: { arg: 0 } }),
];
const lineOf = (text) => {
    const at = ROUTES_TS.indexOf(text);
    assert.ok(at >= 0, `fixture must contain: ${text}`);
    assert.strictEqual(ROUTES_TS.indexOf(text, at + 1), -1, `fixture must contain exactly one: ${text}`);
    return ROUTES_TS.slice(0, at).split('\n').length;
};
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
const PREVIEW = '{ changed: boolean; fields: string[]; }';
const PROJECT = '{ id: string; name: string; }';
/**
 * The same member as the compiler normalises it in a union it inferred from
 * object literals: each member lists the others' properties as optional
 * `undefined`. Narrowing keeps the member the compiler built.
 */
const PROJECT_IN_UNION = '{ changed?: undefined; fields?: undefined; id: string; name: string; }';
const PREVIEW_IN_UNION = '{ changed: boolean; fields: string[]; id?: undefined; name?: undefined; }';
/** Each row: its registration, and optionally the expression a model located. */
const ROWS = {
    flag: { registration: "app.post('/flag',", located: "execute(req.body, 'commit')" },
    flagPreview: { registration: "app.post('/flag/preview',", located: "execute(req.body, 'preview')" },
    flagLine: { registration: "app.post('/flag'," },
    flagPreviewLine: { registration: "app.post('/flag/preview'," },
    things: { registration: "app.get('/things',", located: 'read({ name: req.params.name })' },
    thingParts: {
        registration: "app.get('/things/:id/parts',",
        located: 'read({ name: req.params.name, id: req.params.id })',
    },
    thingsLine: { registration: "app.get('/things'," },
    thingPartsLine: { registration: "app.get('/things/:id/parts'," },
    send: { registration: "app.post('/send',", located: "{ id: 'p1', name: 'n' }", within: 'const executeSend' },
    // The model located the other route's send: the arguments still decide.
    sendPreview: {
        registration: "app.post('/send/preview',",
        located: "{ id: 'p1', name: 'n' }",
        within: 'const executeSend',
    },
    throwCommit: { registration: "app.post('/throw',", located: "{ id: 'p1', name: 'n' }", within: 'const executeThrow' },
    throwPreview: { registration: "app.post('/throw/preview',", located: 'error.preview', within: 'const executeThrow' },
    built: { registration: "app.get('/built',", located: 'readBuilt({ name: req.params.name })' },
    builtParts: {
        registration: "app.get('/built/:id/parts',",
        located: 'readBuilt({ name: req.params.name, id: req.params.id })',
    },
    once: { registration: "app.post('/once'," },
    sameA: { registration: "app.post('/same/a'," },
};
describe('carrick#2372: a response read from an executor routes share', () => {
    let client;
    let repoDir;
    let inferred;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2372-'));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'commonjs',
                moduleResolution: 'node',
                target: 'es2022',
                skipLibCheck: true,
                types: [],
            },
            include: ['src'],
        }));
        const library = path.join(repoDir, 'node_modules', PKG);
        fs.mkdirSync(library, { recursive: true });
        fs.writeFileSync(path.join(library, 'package.json'), JSON.stringify({ name: PKG, version: '1.0.0', types: 'index.d.ts' }));
        fs.writeFileSync(path.join(library, 'index.d.ts'), LIBRARY_DTS);
        const routesPath = path.join(repoDir, 'src', 'routes.ts');
        fs.writeFileSync(routesPath, ROUTES_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init-2372', repo_root: repoDir });
        const res = await client.send({
            action: 'infer',
            request_id: 'infer-2372',
            requests: Object.entries(ROWS).map(([alias, row]) => {
                const registrationLine = lineOf(row.registration);
                // The located expression's line: the first one holding it at or
                // after the executor it sits in, or the registration.
                const from = row.within ? lineOf(row.within) : registrationLine;
                const locatedLine = row.located
                    ? ROUTES_TS.split('\n').findIndex((line, index) => index + 1 >= from && line.includes(row.located)) + 1
                    : undefined;
                return {
                    file_path: routesPath,
                    line_number: registrationLine,
                    infer_kind: 'response_body',
                    alias,
                    ...(row.located ? { expression_text: row.located, expression_line: locatedLine } : {}),
                };
            }),
            server_claims: { from_dir: repoDir, checks: LABELS, type_packages: {} },
        }, 120_000);
        assert.strictEqual(res.status, 'success', JSON.stringify(res));
        inferred = new Map((res.inferred_types ?? []).map((t) => [t.alias, t]));
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    const row = (alias) => {
        const t = inferred.get(alias);
        assert.ok(t, `expected an inferred type for ${alias}; got ${[...inferred.keys()].join(', ')}`);
        return t;
    };
    const text = (alias) => collapse(row(alias).type_string);
    it('narrows a returned executor result by a literal mode argument', () => {
        assert.strictEqual(text('flag'), PROJECT_IN_UNION);
        assert.strictEqual(text('flagPreview'), PREVIEW_IN_UNION);
        assert.strictEqual(text('flagLine'), PROJECT_IN_UNION);
        assert.strictEqual(text('flagPreviewLine'), PREVIEW_IN_UNION);
        for (const alias of ['flag', 'flagPreview', 'flagLine', 'flagPreviewLine']) {
            assert.strictEqual(row(alias).response_modes, undefined, alias);
        }
    });
    it('narrows a declared union return by a member the argument leaves out or states', () => {
        assert.strictEqual(text('things'), '{ items: string[]; }');
        assert.strictEqual(text('thingParts'), '{ id: string; parts: number[]; }');
        assert.strictEqual(text('thingsLine'), '{ items: string[]; }');
        assert.strictEqual(text('thingPartsLine'), '{ id: string; parts: number[]; }');
    });
    it("narrows an executor's sends by the mode its route passes, whichever send was located", () => {
        assert.strictEqual(text('send'), PROJECT);
        assert.strictEqual(text('sendPreview'), PREVIEW);
        assert.strictEqual(row('send').response_modes, undefined);
    });
    it('keeps the union and names the argument when a throw decides the body', () => {
        for (const alias of ['throwCommit', 'throwPreview']) {
            assert.match(text(alias), /changed: boolean/, alias);
            assert.match(text(alias), /id: string; name: string/, alias);
            assert.deepStrictEqual(row(alias).response_modes, { reads: [{ location: 'argument', field: 'mode' }] }, alias);
        }
    });
    it('keeps the union and names the argument when a local function builds the body', () => {
        for (const alias of ['built', 'builtParts']) {
            assert.match(text(alias), /items: string\[\]/, alias);
            assert.match(text(alias), /parts: number\[\]/, alias);
            assert.deepStrictEqual(row(alias).response_modes, { reads: [{ location: 'argument', field: 'id' }] }, alias);
        }
    });
    it('reads no mode from an executor one call reaches', () => {
        // The one caller's literal still narrows its own answer.
        assert.strictEqual(text('once'), PROJECT_IN_UNION);
        assert.strictEqual(row('once').response_modes, undefined);
    });
    it('reads no mode from a literal every caller passes alike', () => {
        // Two returns survive the literal, and they match two members: the union
        // is published whole (a narrowing to one member is all this reads).
        assert.match(text('sameA'), /missing: boolean/);
        assert.match(text('sameA'), /changed: boolean/);
        assert.strictEqual(row('sameA').response_modes, undefined);
    });
});
