/**
 * carrick#3500 step one: "which function body does this call run" is one
 * answer, read by five helpers. An overloaded function (signatures plus one
 * implementation) and a function wrapped in `satisfies` (`(x) => x satisfies
 * Fn`, `const f = ((x) => x) satisfies Fn`) are the same body as the plain
 * declaration, so every helper finds it.
 *
 * The helpers are private to their classes; the tests reach them by name
 * on a project loaded in process.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Node, Project, SyntaxKind } from 'ts-morph';
import { TypeInferrer } from '../src/type-inferrer.js';
import { calledFunction } from '../src/argument-modes.js';
import { StatedParams } from '../src/stated-params.js';
import { functionBodyOf } from '../src/function-body.js';
const SOURCE = `type Fn = (x: { a: number }) => { a: number };

// Overloaded: two signatures and one implementation.
function over(x: { a: number }): { a: number };
function over(x: { a: number }, y: string): { a: number };
function over(x: { a: number }, _y?: string): { a: number } {
  return x;
}

// The arrow itself carries satisfies.
const sat = ((x: { a: number }) => x) satisfies Fn;
// The body returns its argument through satisfies.
const inner = (x: { a: number }) => x satisfies { a: number };
// Plain control.
function plain(x: { a: number }): { a: number } {
  return x;
}
const twice = (x: { a: number }) => x;

over({ a: 1 });
sat({ a: 2 });
inner({ a: 3 });
plain({ a: 4 });
twice({ a: 5 });
`;
describe('carrick#3500: the function body a call runs, through every helper', () => {
    let repoDir;
    let project;
    let file;
    let inferrer;
    before(() => {
        repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'called-function-')));
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({ compilerOptions: { strict: true, target: 'es2022', module: 'esnext', types: [] }, include: ['*.ts'] }));
        fs.writeFileSync(path.join(repoDir, 'mod.ts'), SOURCE);
        project = new Project({ tsConfigFilePath: path.join(repoDir, 'tsconfig.json') });
        file = project.getSourceFileOrThrow(path.join(repoDir, 'mod.ts'));
        inferrer = new TypeInferrer({ project, repoRoot: repoDir });
    });
    after(() => {
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    const callTo = (name) => {
        const call = file
            .getDescendantsOfKind(SyntaxKind.CallExpression)
            .find((c) => c.getExpression().getText() === name && Node.isExpressionStatement(c.getParent()));
        assert.ok(call, `fixture calls ${name}`);
        return call;
    };
    const bodyLine = (node) => node?.getStartLineNumber();
    const lineOf = (needle) => SOURCE.slice(0, SOURCE.indexOf(needle)).split('\n').length;
    const stated = () => new StatedParams({ sends: new Set(), sets: new Set(), bodies: new Set(), returns: new Set() }, () => true, (() => undefined), (() => undefined), (() => undefined));
    const OVER_IMPL = lineOf('function over(x: { a: number }, _y');
    const SAT_ARROW = lineOf('const sat');
    const INNER_ARROW = lineOf('const inner');
    describe('calledFunction', () => {
        it('finds the implementation of an overloaded function', () => {
            assert.strictEqual(bodyLine(calledFunction(callTo('over'))), OVER_IMPL);
        });
        it('finds an arrow wrapped in satisfies', () => {
            assert.strictEqual(bodyLine(calledFunction(callTo('sat'))), SAT_ARROW);
        });
    });
    describe('repoFunction', () => {
        it('finds the implementation of an overloaded function', () => {
            const fn = stated()['repoFunction'](callTo('over').getExpression());
            assert.strictEqual(bodyLine(fn), OVER_IMPL);
        });
        it('finds an arrow wrapped in satisfies', () => {
            const fn = stated()['repoFunction'](callTo('sat').getExpression());
            assert.strictEqual(bodyLine(fn), SAT_ARROW);
        });
    });
    describe('builderFunctionOf', () => {
        it('finds the implementation of an overloaded function', () => {
            const symbol = callTo('over').getExpression().getSymbol();
            assert.strictEqual(bodyLine(inferrer['builderFunctionOf'](symbol)), OVER_IMPL);
        });
        it('finds an arrow wrapped in satisfies', () => {
            const symbol = callTo('sat').getExpression().getSymbol();
            assert.strictEqual(bodyLine(inferrer['builderFunctionOf'](symbol)), SAT_ARROW);
        });
    });
    describe('passThroughFunction', () => {
        it('finds the implementation of an overloaded function', () => {
            assert.strictEqual(bodyLine(inferrer['passThroughFunction'](callTo('over'), 0)), OVER_IMPL);
        });
        it('finds an arrow wrapped in satisfies', () => {
            assert.strictEqual(bodyLine(inferrer['passThroughFunction'](callTo('sat'), 0)), SAT_ARROW);
        });
        it('reads a returned `x satisfies T` as the argument', () => {
            assert.strictEqual(bodyLine(inferrer['passThroughFunction'](callTo('inner'), 0)), INNER_ARROW);
        });
    });
    describe('asHandlerFunction', () => {
        it('finds the implementation of an overloaded function', () => {
            const name = callTo('over').getExpression();
            assert.strictEqual(bodyLine(inferrer['asHandlerFunction'](name)), OVER_IMPL);
        });
        it('finds an arrow wrapped in satisfies', () => {
            const name = callTo('sat').getExpression();
            assert.strictEqual(bodyLine(inferrer['asHandlerFunction'](name)), SAT_ARROW);
        });
        it('finds an inline arrow wrapped in satisfies', () => {
            const wrapped = file.getVariableDeclarationOrThrow('sat').getInitializerOrThrow();
            assert.strictEqual(bodyLine(inferrer['asHandlerFunction'](wrapped)), SAT_ARROW);
        });
    });
    describe('functionBodyOf', () => {
        it('answers the one body among declarations, and nothing for none or two', () => {
            const over = file.getFunctionOrThrow('over');
            assert.strictEqual(functionBodyOf(over.getSymbol().getDeclarations())?.getStartLineNumber(), OVER_IMPL);
            assert.strictEqual(functionBodyOf(over.getOverloads()), undefined);
            const both = [file.getFunctionOrThrow('plain'), file.getVariableDeclarationOrThrow('twice')];
            assert.strictEqual(functionBodyOf(both), undefined);
        });
    });
});
