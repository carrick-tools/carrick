/**
 * One JSON wire rule, two implementations (carrick-cloud#3503 T2).
 *
 * The judge compares an `http` payload's sending side as `JsonWire<T>`, a
 * type the compiler evaluates inside the probe (`jsonWireDeclarations`). The
 * stored text prints the same side with the structural printer's
 * `wire: 'json'` mode (`jsonWireType`). Neither can call the other: one is a
 * TypeScript type, the other walks a `Type`. This pins them together: for
 * each shape, the printed text and `JsonWire<T>` are mutually assignable in
 * one compiler program, so the stored text says what the judge compared.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Project } from 'ts-morph';
import { expandTypeStructural } from '../src/type-structural-expander.js';
import { jsonWireDeclarations } from '../src/capture/index.js';
import { expandOriginOf } from './helpers.js';

const DECLARATIONS = `
  declare class Inner { toJSON(): object; }
  declare class Opaque { toJSON(): any; }
  declare class Stamp { toJSON(): { at: Date; n: number }; }
  interface Nested { inner: Inner; stamp: Stamp; when: Date }
  export type T_date = Date;
  export type T_deep_dates = Date[][][];
  export type T_nested_class = { outer: Nested; list: Stamp[] };
  export type T_to_json_any = { o: Opaque };
  export type T_bigint = { size: bigint };
  export type T_union_null = { at: Date | null; s: string } | null;
`;

const CASES = [
  'T_date',
  'T_deep_dates',
  'T_nested_class',
  'T_to_json_any',
  'T_bigint',
  'T_union_null',
];

describe('the printer\'s JSON wire form is the judge\'s JsonWire<T> (carrick-cloud#3503 T2)', () => {
  it('each printed wire text and JsonWire<T> are mutually assignable', () => {
    const project = new Project({
      useInMemoryFileSystem: true,
      compilerOptions: { strict: true },
    });
    const source = project.createSourceFile('types.ts', DECLARATIONS);
    const origin = expandOriginOf(project);
    const printed = new Map(
      CASES.map((name) => [
        name,
        expandTypeStructural(source.getTypeAliasOrThrow(name).getType(), origin, {
          wire: 'json',
        }),
      ]),
    );

    const parity = project.createSourceFile(
      'parity.ts',
      [
        DECLARATIONS,
        ...jsonWireDeclarations(''),
        ...CASES.flatMap((name) => [
          `type Printed_${name} = ${printed.get(name)};`,
          `const same_${name}: JsonWireSame<Printed_${name}, JsonWire<${name}>> = true;`,
        ]),
      ].join('\n'),
    );
    const diagnostics = parity.getPreEmitDiagnostics().map((d) => {
      const message = d.getMessageText();
      return `${d.getLineNumber()}: ${typeof message === 'string' ? message : message.getMessageText()}`;
    });
    assert.deepStrictEqual(
      diagnostics,
      [],
      `printed: ${JSON.stringify(Object.fromEntries(printed), null, 2)}`,
    );
    // The cases must exercise the transform, not pass vacuously.
    assert.strictEqual(printed.get('T_date'), 'string');
    assert.doesNotMatch(printed.get('T_deep_dates')!, /Date/);
    assert.doesNotMatch(printed.get('T_nested_class')!, /Date|toJSON/);
  });
});
