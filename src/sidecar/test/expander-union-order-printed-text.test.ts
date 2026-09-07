/**
 * The canonical union order must hold inside the COMPILER'S OWN PRINT
 * (carrick#775, #760).
 *
 * `expandTypeStructural` orders every union it walks, and #778 made the depth
 * backstop do the same. Neither reaches the unions that never get walked at
 * all: the walk hands a whole subtree to `namedText` when the type is a
 * library type kept by name, when an object has no named properties to
 * descend into (an index signature alone), and at tuples, functions and
 * cycles. Everything inside those prints stays in type-id order, which is
 * creation order.
 *
 * That is what the 0.3.45 index shows: 12 of the largest service's 40 literal
 * unions still printed in creation order after #778, and every one of them sits
 * inside a compiler print of one of these two shapes — a library generic's
 * instantiation, or an index-signature body.
 *
 * The perturbation is the instrument from `expander-union-order.test.ts`: one
 * source, two aliases spelling the same literal set in different orders,
 * resolved in both orders over two fresh programs. Whichever declaration the
 * checker reaches first interns the literals and so decides the printed order
 * of BOTH — unless the print is normalised.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Project, type SourceFile } from 'ts-morph';
import {
  expandTypeStructural,
  namedText,
} from '../src/type-structural-expander.js';

const MEMBERS_A = `'sum' | 'count' | 'min'`;
const MEMBERS_B = `'min' | 'sum' | 'count'`;

/** A library declaration, so `isLibraryType` keeps its instantiation by name. */
const LIB = `
export interface Enum<Values> {
  _def: Values;
  parse(input: unknown): Values;
}
`;

/**
 * `Widgets*` is an index-signature-only object: no named properties, so the
 * walk has nothing to descend into and prints the whole body.
 * `Coded*` puts the same union inside a library generic's argument.
 * `Marker` is the fixture guard — it has members, so anything the walk really
 * expanded would show its shape rather than its name.
 */
function source(members: string, suffix: string): string {
  return `
import { Enum } from 'lib';
interface Marker { tag: string; note: string }
export type Widgets${suffix} = { [key: string]: { mode: ${members}; marker: Marker } };
export type Coded${suffix} = { code: Enum<${members}>; marker: Marker };
`;
}

function projectWith(): SourceFile {
  const project = new Project({
    useInMemoryFileSystem: true,
    compilerOptions: { strict: true },
  });
  project.createSourceFile('/node_modules/lib/index.d.ts', LIB);
  project.createSourceFile('/node_modules/lib/package.json', '{"types":"index.d.ts"}');
  return project.createSourceFile(
    '/surface.ts',
    [source(MEMBERS_A, 'A'), source(MEMBERS_B, 'B')].join('\n'),
  );
}

/** Expand the named aliases in the given order, over a fresh program. */
function expandInOrder(aliases: string[]): Map<string, string> {
  const sf = projectWith();
  const out = new Map<string, string>();
  for (const alias of aliases) {
    out.set(alias, expandTypeStructural(sf.getTypeAliasOrThrow(alias).getType()));
  }
  return out;
}

const ALIASES = ['WidgetsA', 'WidgetsB', 'CodedA', 'CodedB'];

describe('union order inside a compiler print (#775)', () => {
  it('is a fixture whose unions really are inside a compiler print', () => {
    // Without this guard the cases below would exercise the walked path, which
    // #761 already made canonical, and would pass whatever `namedText` does.
    const expanded = expandInOrder(ALIASES);

    // The index-signature body has no named properties, so the walk stops at
    // the object itself and the compiler prints the whole subtree. `Marker`
    // has members: printed by NAME, it proves the walk never went inside.
    for (const alias of ['WidgetsA', 'WidgetsB']) {
      const text = expanded.get(alias)!;
      assert.ok(
        /marker: Marker\b/.test(text),
        `${alias} no longer reaches a compiler print — Marker was expanded:\n${text}`,
      );
    }

    // The library generic is kept by name, so its ARGUMENT — where the union
    // sits — is inside the compiler's print of the instantiation. Its own
    // members (`_def`, `parse`) must not appear, or the walk went in and this
    // case proves nothing about `namedText`. `marker` beside it IS expanded,
    // which is the walked path working normally.
    for (const alias of ['CodedA', 'CodedB']) {
      const text = expanded.get(alias)!;
      assert.match(text, /code: Enum</, `Enum was inlined in ${alias}:\n${text}`);
      assert.ok(
        !text.includes('_def'),
        `the library type was walked into in ${alias}:\n${text}`,
      );
      assert.match(
        text,
        /marker: \{ tag: string; note: string; \}/,
        `${alias} should still expand a local named type:\n${text}`,
      );
    }
  });

  it('orders a union inside an index-signature body', () => {
    assert.match(
      expandInOrder(['WidgetsA']).get('WidgetsA')!,
      /mode: "count" \| "min" \| "sum"/,
    );
  });

  it("orders a union inside a library generic's argument", () => {
    assert.match(
      expandInOrder(['CodedA']).get('CodedA')!,
      /Enum<"count" \| "min" \| "sum">/,
    );
  });

  it('prints identically under a perturbed creation order', () => {
    const forward = expandInOrder(ALIASES);
    const reversed = expandInOrder([...ALIASES].reverse());
    for (const alias of ALIASES) {
      assert.strictEqual(
        forward.get(alias),
        reversed.get(alias),
        `${alias} must print identically whichever alias is resolved first`,
      );
    }
  });

  it('agrees with the walked path on the same union', () => {
    // The claim the whole change rests on: a union prints ONE way, whichever
    // side of a `namedText` boundary it happens to fall. Same type, both
    // paths — the walk that renders each member itself, and the compiler's
    // print put through the text rule — must be byte-identical.
    const project = new Project({
      useInMemoryFileSystem: true,
      compilerOptions: { strict: true },
    });
    const mixed = project
      .createSourceFile(
        '/mixed.ts',
        `export type Mixed = 'b' | null | 'a' | number | { z: string };`,
      )
      .getTypeAliasOrThrow('Mixed')
      .getType();

    assert.strictEqual(expandTypeStructural(mixed), namedText(mixed));
  });

  it('keeps every member of a printed union', () => {
    const text = expandInOrder(['CodedA']).get('CodedA')!;
    for (const member of ['"sum"', '"count"', '"min"']) {
      assert.ok(text.includes(member), `${member} missing:\n${text}`);
    }
  });
});
