/**
 * carrick#4187: a member the checker synthesised has no name node, so the
 * printer fell back to the bare symbol name and wrote `{ a-b: number }`, which
 * TypeScript cannot parse. A key that is not an identifier must be quoted.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Project, ts } from 'ts-morph';
import { expandTypeStructural } from '../src/type-structural-expander.js';
import { expandOriginOf } from './helpers.js';

const SOURCE = `
  export type Keys = 'a-b' | '1st' | 'plain';
  export type Mapped = { [K in Keys]: number };
`;

describe('non-identifier keys are quoted (carrick#4187)', () => {
  const project = new Project({ useInMemoryFileSystem: true, compilerOptions: { strict: true } });
  const sf = project.createSourceFile('/surface.ts', SOURCE);
  const type = sf.getTypeAliasOrThrow('Mapped').getType();

  it('the fixture members carry no declaration', () => {
    for (const prop of type.getProperties()) assert.strictEqual(prop.getDeclarations().length, 0);
  });

  it('prints quoted keys that parse', () => {
    const printed = expandTypeStructural(type, expandOriginOf(project));
    assert.ok(printed.includes('"a-b": number'), printed);
    assert.ok(printed.includes('"1st": number'), printed);
    assert.ok(printed.includes('plain: number'), printed);
    const parsed = ts.createSourceFile('x.ts', `type T = ${printed};`, ts.ScriptTarget.Latest, true) as ts.SourceFile & {
      parseDiagnostics: ts.Diagnostic[];
    };
    assert.strictEqual(parsed.parseDiagnostics.length, 0, printed);
  });
});
