/**
 * carrick#1433: a member the CHECKER synthesised has no declaration of its own,
 * and `Symbol.getDeclaredType()` answers `any` for such a symbol — so the
 * printer published `{ id: any; createdAt: any }` for a type the compiler had
 * fully resolved, and every row carrying one was demoted for a top type that
 * is not in the contract.
 *
 * A mapped type is where these come from in real code: a query builder's
 * projection, a `Pick`, anything of the `{ [K in Keys]: … }` shape. The test
 * asserts the members really do carry no declaration before asserting the
 * print, because a mapped type that is NOT in that state would exercise the
 * declaration branch instead and pass whatever this one does.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Project } from 'ts-morph';
import { expandTypeStructural } from '../src/type-structural-expander.js';
import { expandOriginOf } from './helpers.js';

const SOURCE = `
  interface Row { id: string; name: string; createdAt: Date; secret: boolean }
  type Column = 'id' | 'name' | 'createdAt';
  type ValueOf<K extends Column> = K extends 'createdAt' ? Date : string;
  /** What a projection helper returns: members the checker computes. */
  export type Projection = { [K in Column]: ValueOf<K> };
  /** The same state through the simplest mapping over another type. */
  export type Picked = { [K in 'id' | 'secret']: Row[K] };
  /** The wire mapping (carrick#1163) declared by a mapped type, so the
   *  \`toJSON\` member \`jsonWireType\` looks for is synthesised too. */
  export type Wire = { [K in 'toJSON']: () => { id: string } };
`;

function projectWithSource(): Project {
  const project = new Project({
    useInMemoryFileSystem: true,
    compilerOptions: { strict: true },
  });
  project.createSourceFile('/surface.ts', SOURCE);
  return project;
}

describe('a synthesised member prints its real type (carrick#1433)', () => {
  const project = projectWithSource();
  const sf = project.getSourceFileOrThrow('/surface.ts');
  const expand = (alias: string) =>
    expandTypeStructural(sf.getTypeAliasOrThrow(alias).getType(), expandOriginOf(project));

  it('the fixture members really carry no declaration', () => {
    for (const alias of ['Projection', 'Picked', 'Wire']) {
      const props = sf.getTypeAliasOrThrow(alias).getType().getProperties();
      assert.ok(props.length > 0, `${alias} has members`);
      for (const prop of props) {
        assert.strictEqual(
          prop.getDeclarations().length,
          0,
          `${alias}.${prop.getName()} must be a synthesised member for this fixture to test anything`,
        );
        // The wrong read, named: this is what the print used to carry.
        assert.strictEqual(prop.getDeclaredType().getText(), 'any');
      }
    }
  });

  it('prints the computed member types, not any', () => {
    const printed = expand('Projection');
    assert.strictEqual(printed, '{ id: string; name: string; createdAt: Date; }', printed);
  });

  it('prints a mapping over another type the same way', () => {
    const printed = expand('Picked');
    assert.strictEqual(printed, '{ id: string; secret: boolean; }', printed);
  });

  it('reads a wire mapping whose toJSON member is synthesised (carrick#1163)', () => {
    const type = sf.getTypeAliasOrThrow('Wire').getType();
    const printed = expandTypeStructural(type, expandOriginOf(project), { wire: 'json' });
    // Without the fix `jsonWireType` reads `any` for the member, finds no call
    // signature on it, and prints the carrier instead of what it sends.
    assert.strictEqual(printed, '{ id: string; }', printed);
  });
});
