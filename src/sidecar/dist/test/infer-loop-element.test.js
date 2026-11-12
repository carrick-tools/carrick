/**
 * carrick#1029: a route registered once per element of a constant list is
 * typed for the element its row's path names, not as the union of every
 * element's payload.
 *
 * The fixture registers routes in a `for … of` over a tuple list written in
 * place (one line), in a `.forEach` over a `const` list (several lines), and in
 * a loop over a list a function returns, which the code does not state. The
 * scanner sends the row's own path (`route_path`); the inferrer types the one
 * handler with the list rewritten to the named element.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { TypeInferrer } from '../src/type-inferrer.js';
import { endsWithPath } from '../src/loop-element.js';
const ROUTES = `interface Alpha {
  alphaId: number;
}
interface Beta {
  betaName: string;
}
interface App {
  get(path: string, handler: () => Promise<unknown>): void;
}
declare const app: App;
declare function rows(): { alpha: Alpha[]; beta: Beta[] };
declare function listKinds(): Array<'alpha' | 'beta'>;

for (const [path, key] of [['alpha-rows', 'alpha'], ['beta-rows', 'beta']] as const) app.get(\`/items/\${path}\`, async () => ({ items: rows()[key], total: 1 }));

const KINDS = [
  'alpha',
  'beta',
] as const;
KINDS.forEach((kind) => {
  app.get(\`/kinds/\${kind}\`, async () => ({ items: rows()[kind] }));
});

for (const kind of listKinds()) {
  app.get(\`/listed/\${kind}\`, async () => ({ items: rows()[kind] }));
}

app.get('/status', async () => ({ state: rows().alpha.length > 0 ? 'up' : 'down' }));
`;
function lineOf(text, needle) {
    const index = text.indexOf(needle);
    assert.ok(index >= 0, `fixture holds ${needle}`);
    return text.slice(0, index).split('\n').length;
}
/** The type text with whitespace collapsed. */
function collapse(text) {
    return text?.replace(/\s+/g, ' ');
}
describe('carrick#1029: a loop-registered route is typed for its own element', () => {
    let repoDir;
    let routesPath;
    before(() => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'loop-element-'));
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({ compilerOptions: { strict: true, target: 'es2022', module: 'esnext', types: [] }, include: ['*.ts'] }));
        routesPath = path.join(repoDir, 'routes.ts');
        fs.writeFileSync(routesPath, ROUTES);
    });
    after(() => {
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    const read = (requests) => {
        const project = new Project({ tsConfigFilePath: path.join(repoDir, 'tsconfig.json') });
        const inferrer = new TypeInferrer({ project, repoRoot: repoDir });
        const res = inferrer.infer(requests);
        const text = project.getSourceFileOrThrow(routesPath).getFullText();
        return {
            text,
            order: (res.inferred_types ?? []).map((t) => t.alias),
            byAlias: new Map((res.inferred_types ?? []).map((t) => [t.alias, collapse(t.type_string)])),
            unwidened: new Map((res.inferred_types ?? []).map((t) => [t.alias, collapse(t.unwidened_type_string)])),
        };
    };
    const route = (alias, needle, routePath) => ({
        file_path: routesPath,
        line_number: lineOf(ROUTES, needle),
        infer_kind: 'response_body',
        alias,
        ...(routePath ? { route_path: routePath } : {}),
    });
    it('types each element of a tuple list written in place, under a mount prefix', () => {
        const { byAlias, text } = read([
            route('alpha', 'app.get(`/items/', '/api/v1/items/alpha-rows'),
            route('beta', 'app.get(`/items/', '/api/v1/items/beta-rows'),
        ]);
        assert.strictEqual(byAlias.get('alpha'), '{ items: { alphaId: number; }[]; total: number; }');
        assert.strictEqual(byAlias.get('beta'), '{ items: { betaName: string; }[]; total: number; }');
        // The file the scan loaded is the file left behind.
        assert.strictEqual(text, ROUTES);
    });
    it('types each element of a const list a forEach registers over several lines', () => {
        const { byAlias } = read([
            route('beta', 'app.get(`/kinds/', '/kinds/beta'),
            // The handler's return asked for, as a row whose return is its payload is.
            {
                ...route('alpha', 'app.get(`/kinds/', '/kinds/alpha'),
                infer_kind: 'function_return',
                expression_text: '{ items: rows()[kind] }',
                expression_line: lineOf(ROUTES, 'app.get(`/kinds/'),
            },
        ]);
        assert.strictEqual(byAlias.get('beta'), '{ items: { betaName: string; }[]; }');
        assert.strictEqual(byAlias.get('alpha'), '{ items: { alphaId: number; }[]; }');
    });
    it('keeps the union where the row states no path, or names no element', () => {
        const { byAlias } = read([
            route('unsent', 'app.get(`/items/'),
            route('template', 'app.get(`/items/', '/items/:path'),
            route('elsewhere', 'app.get(`/items/', '/other/alpha-rows'),
        ]);
        for (const alias of ['unsent', 'template', 'elsewhere']) {
            assert.strictEqual(byAlias.get(alias), '{ items: { alphaId: number; }[] | { betaName: string; }[]; total: number; }', alias);
        }
    });
    it("leaves the file's other readings whole, and answers in request order", () => {
        // The status route is read unwidened in the same file the loop rows rewrite.
        const { byAlias, unwidened, order } = read([
            route('alpha', 'app.get(`/items/', '/items/alpha-rows'),
            route('status', "app.get('/status'"),
            route('beta', 'app.get(`/items/', '/items/beta-rows'),
        ]);
        assert.deepStrictEqual(order, ['alpha', 'status', 'beta']);
        assert.strictEqual(byAlias.get('status'), '{ state: string; }');
        assert.match(unwidened.get('status') ?? '', /^\{ state: ("up" \| "down"|"down" \| "up"); \}$/);
        assert.strictEqual(byAlias.get('beta'), '{ items: { betaName: string; }[]; total: number; }');
    });
    it('keeps the union for a list the code does not state', () => {
        const { byAlias } = read([route('listed', 'app.get(`/listed/', '/listed/alpha')]);
        assert.strictEqual(byAlias.get('listed'), '{ items: { alphaId: number; }[] | { betaName: string; }[]; }');
    });
    it('matches a path by its last segments, a parameter matching a parameter of any name', () => {
        assert.ok(endsWithPath('/api/v1/teams/{name}/reports/a', '/teams/:name/reports/a'));
        assert.ok(endsWithPath('/x/a', '/x/a'));
        assert.ok(!endsWithPath('/x/a', '/x/b'));
        assert.ok(!endsWithPath('/a', '/x/a'));
        assert.ok(!endsWithPath('/x/:id', '/x/a'));
    });
});
