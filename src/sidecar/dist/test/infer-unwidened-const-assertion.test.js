/**
 * carrick#2343: a literal the source already asserts `as const` is not marked
 * again by the unwidened reading (carrick#1516).
 *
 * Marking it again writes `('warn' as const) as const`, which the compiler
 * rejects (a const assertion applies to a literal only). The reading drops on
 * any diagnostic it adds, so one such literal anywhere on a handler's path
 * cost the whole reading, and the check had nothing to read a second time
 * (carrick#1538).
 *
 * The shapes: a const-asserted literal inside an element of a list the
 * handler returns, beside a discriminant that widens; the same assertion
 * through parentheses and in angle-bracket form; a const-asserted list; and an
 * assertion to a wider type, which still takes a marked literal.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { TypeInferrer } from '../src/type-inferrer.js';
const DECORATORS = `export function Post(path: string): MethodDecorator {
  return () => {
    void path;
  };
}
`;
// Line numbers are read off this text by `lineOf`.
const CONTROLLER = `import { Post } from './decorators';

interface Check {
  ruleId: string;
  severity: 'fail' | 'warn';
}

export class PlanController {
  private base: Check[] = [];
  private compiled = true;

  @Post('preview')
  preview() {
    const checks = this.base.length > 0
      ? [...this.base, { ruleId: 'schema-v1', severity: 'warn' as const }]
      : this.base;
    if (!this.compiled) return { ok: false, checksum: null, checks };
    return { ok: true, checksum: 'abc', checks };
  }

  @Post('forms')
  forms() {
    const mode = (('strict')) as const;
    const tag = <const>'beta';
    const kinds = ['a', 'b'] as const;
    const label = 'x' as string;
    return { state: this.compiled ? 'ready' : 'empty', mode, tag, kinds, label };
  }
}
`;
function lineOf(text, needle) {
    const at = text.indexOf(needle);
    assert.ok(at >= 0, `fixture must contain: ${needle}`);
    return text.slice(0, at).split('\n').length;
}
const collapse = (text) => text?.replace(/\s+/g, ' ').trim();
describe('carrick#2343: a literal already asserted as const is not marked again', () => {
    let repoDir;
    let readings;
    before(() => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2343-'));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                experimentalDecorators: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        fs.writeFileSync(path.join(repoDir, 'src', 'decorators.ts'), DECORATORS);
        const controllerPath = path.join(repoDir, 'src', 'plan.controller.ts');
        fs.writeFileSync(controllerPath, CONTROLLER);
        const route = (name) => ({
            file_path: controllerPath,
            line_number: lineOf(CONTROLLER, `@Post('${name}')`),
            infer_kind: 'response_body',
            alias: name,
        });
        const project = new Project({ tsConfigFilePath: path.join(repoDir, 'tsconfig.json') });
        const inferrer = new TypeInferrer({ project, repoRoot: repoDir });
        const result = inferrer.infer([route('preview'), route('forms')]);
        readings = new Map((result.inferred_types ?? []).map((t) => [t.alias, t]));
    });
    after(() => {
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    it('reads a handler whose list element holds a const-asserted literal', () => {
        const t = readings.get('preview');
        assert.ok(t, 'the route must infer');
        assert.match(collapse(t.type_string), /ok: boolean/, 'the published type stays widened');
        const unwidened = collapse(t.unwidened_type_string);
        assert.ok(unwidened, `a reading is expected, got: ${JSON.stringify(t)}`);
        assert.match(unwidened, /ok: false; checksum: null/);
        assert.match(unwidened, /ok: true; checksum: "abc"/);
        assert.match(unwidened, /severity: "fail" \| "warn"|severity: "warn" \| "fail"/, 'the asserted literal keeps its own type');
    });
    it('reads through parentheses, angle brackets, a const list and a wider assertion', () => {
        const t = readings.get('forms');
        assert.ok(t, 'the route must infer');
        assert.match(collapse(t.type_string), /state: string/);
        const unwidened = collapse(t.unwidened_type_string);
        assert.ok(unwidened, `a reading is expected, got: ${JSON.stringify(t)}`);
        assert.match(unwidened, /state: "ready" \| "empty"|state: "empty" \| "ready"/);
        assert.match(unwidened, /mode: "strict"/);
        assert.match(unwidened, /tag: "beta"/);
        assert.match(unwidened, /kinds: readonly \["a", "b"\]/);
        assert.match(unwidened, /label: string/, 'an assertion to a wider type is the contract');
    });
});
