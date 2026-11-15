/**
 * carrick#4403 T-B2: one table over `resolve_definitions` pinning the stored
 * text of each GraphQL leaf by its SDL scalar. The server sends a leaf through
 * its scalar's `serialize`: a built-in scalar fixes the sent type whatever the
 * resolver returns (a `Date` in an `Int` field is a number), and any other leaf
 * (a custom scalar, an enum, a field the table does not name) keeps the JSON
 * form. The producer's own `null` is kept.
 */

import { describe, it, before } from 'node:test';
import * as assert from 'node:assert';
import { DefinitionResolver } from '../src/definition-resolver.js';
import type { GraphqlLeaf, GraphqlTypes } from '../src/capture/api.js';
import { stubDirFor } from './helpers.js';

interface Row {
  name: string;
  declared: string;
  leaf: GraphqlLeaf | undefined;
  /** The printed field type, after `field: `. */
  expected: string;
}

const ROWS: Row[] = [
  { name: 'String over a Date', declared: 'Date', leaf: { scalar: 'String' }, expected: 'string' },
  { name: 'String over a bigint', declared: 'bigint', leaf: { scalar: 'String' }, expected: 'string' },
  { name: 'ID over a bigint', declared: 'bigint', leaf: { scalar: 'ID' }, expected: 'string' },
  { name: 'Int over a Date', declared: 'Date', leaf: { scalar: 'Int' }, expected: 'number' },
  { name: 'Float over a Date', declared: 'Date', leaf: { scalar: 'Float' }, expected: 'number' },
  { name: 'Int over a bigint', declared: 'bigint', leaf: { scalar: 'Int' }, expected: 'number' },
  { name: 'Boolean over a string', declared: 'string', leaf: { scalar: 'Boolean' }, expected: 'boolean' },
  { name: 'non-null String over a Date', declared: 'Date', leaf: { non_null: { scalar: 'String' } }, expected: 'string' },
  { name: 'String keeps the producer null', declared: 'bigint | null', leaf: { scalar: 'String' }, expected: 'null | string' },
  { name: 'a list of Int maps each element', declared: 'Date[]', leaf: { list: { scalar: 'Int' } }, expected: 'number[]' },
  { name: 'a custom scalar keeps the JSON form of a Date', declared: 'Date', leaf: { scalar: 'DateTime' }, expected: 'string' },
  { name: 'a custom scalar leaves a bigint alone', declared: 'bigint', leaf: { scalar: 'Long' }, expected: 'bigint' },
  { name: 'an enum keeps the JSON form', declared: 'Date', leaf: { enum: 'Color' }, expected: 'string' },
  { name: 'a field the table does not name keeps the JSON form', declared: 'Date', leaf: undefined, expected: 'string' },
  { name: 'a deep unknown is not laundered into String', declared: 'unknown', leaf: { scalar: 'String' }, expected: 'unknown' },
  { name: 'a deep any is not laundered into Int', declared: 'any', leaf: { scalar: 'Int' }, expected: 'any' },
];

describe('resolve_definitions prints each GraphQL leaf in its scalar\'s sent type (carrick#4403)', () => {
  let stubDir: string;
  let types: GraphqlTypes;
  before(() => {
    const fields = ROWS.map((r, i) => `  f${i}: ${r.declared};`).join('\n');
    stubDir = stubDirFor(
      `export interface Payload {\n${fields}\n}\nexport type Endpoint_payload = Payload;\n`,
    );
    const sdlFields: { [f: string]: GraphqlLeaf } = {};
    ROWS.forEach((r, i) => {
      if (r.leaf !== undefined) sdlFields[`f${i}`] = r.leaf;
    });
    types = {
      Query: { fields: { payload: { non_null: { type: 'Payload' } } } },
      Payload: { fields: sdlFields },
    };
  });

  it('pins every row of the table', () => {
    const [d] = new DefinitionResolver().resolveFromStub(
      stubDir,
      [
        {
          alias: 'Endpoint_payload',
          protocol: 'graphql',
          type_kind: 'response',
          side: 'producer',
          procedure: false,
          graphql_root: { type: 'Query', field: 'payload' },
        },
      ],
      types,
    );
    assert.ok(d, 'Endpoint_payload resolved');
    const failures: string[] = [];
    ROWS.forEach((r, i) => {
      const got = new RegExp(`\\bf${i}\\??: ([^;]+);`).exec(d.expanded)?.[1];
      if (got !== r.expected) failures.push(`${r.name}: expected ${r.expected}, got ${got}`);
    });
    assert.deepStrictEqual(failures, [], d.expanded);
  });
});
