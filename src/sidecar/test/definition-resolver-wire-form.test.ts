/**
 * carrick-cloud#3503: the stored structural text of an `http` payload's
 * sending side says what travels.
 *
 * `JSON.stringify` writes a value's `toJSON()` result, so a producer's `Date`
 * reaches the consumer as a string. The judge already compares the sending
 * side of an `http` pair in that form (`JsonWire<T>` in the probe). The
 * definitions pass printed the same alias as declared, so the stored
 * `expanded` text said `Date` where the judge read `string`. Both now read
 * one rule, `wireFormFor` (a GraphQL producer response too, carrick#4275).
 *
 * `definition` (as written) keeps the declared names in every case.
 */

import { describe, it, before } from 'node:test';
import * as assert from 'node:assert';
import { DefinitionResolver } from '../src/definition-resolver.js';
import type { DefinitionAlias } from '../src/types.js';
import { stubDirFor } from './helpers.js';

const STUB = `export interface Row {
  id: string;
  createdAt: Date;
  tags: { at: Date }[];
}
export declare class Money {
  cents: bigint;
  toJSON(): { v: number };
}
export interface Priced {
  price: Money;
  size: bigint;
}
export type Endpoint_row = Row;
export type Endpoint_priced = Priced;
`;

describe('resolve_definitions prints the sending side of an http payload in wire form (carrick-cloud#3503)', () => {
  let stubDir: string;
  before(() => {
    stubDir = stubDirFor(STUB);
  });

  function resolve(
    payload: Omit<DefinitionAlias, 'alias' | 'procedure'> & { procedure?: boolean },
    alias = 'Endpoint_row',
  ) {
    const [definition] = new DefinitionResolver().resolveFromStub(stubDir, [
      { alias, procedure: false, ...payload },
    ]);
    assert.ok(definition, `${alias} resolved`);
    return definition;
  }

  it('an http producer response travels as its JSON form, nested Dates included', () => {
    const d = resolve({ protocol: 'http', type_kind: 'response', side: 'producer' });
    assert.match(d.expanded, /createdAt: string/, d.expanded);
    assert.match(d.expanded, /at: string/, d.expanded);
    assert.doesNotMatch(d.expanded, /Date/, d.expanded);
    assert.match(d.definition, /createdAt: Date/, 'the as-written form keeps the declared type');
  });

  it('an http consumer request body is the consumer\'s to send, so it travels as JSON too', () => {
    const d = resolve({ protocol: 'http', type_kind: 'request', side: 'consumer' });
    assert.doesNotMatch(d.expanded, /Date/, d.expanded);
  });

  it('an RPC procedure (tRPC and similar) keeps its declared Date: its transformer decides the wire form', () => {
    // A router with a transformer that revives dates (superjson) sends a
    // `Date` as a `Date`; until the transformer is a known fact the stored
    // text fails closed to the declared form (carrick#4275).
    const response = resolve({ protocol: 'http', type_kind: 'response', side: 'producer', procedure: true });
    assert.match(response.expanded, /createdAt: Date/, response.expanded);
    const input = resolve({ protocol: 'http', type_kind: 'request', side: 'consumer', procedure: true });
    assert.match(input.expanded, /createdAt: Date/, input.expanded);
  });

  it('a GraphQL producer response travels as its JSON form: the server serialises each leaf (carrick#4275)', () => {
    const d = resolve({ protocol: 'graphql', type_kind: 'response', side: 'producer' });
    assert.match(d.expanded, /createdAt: string/, d.expanded);
    assert.doesNotMatch(d.expanded, /Date/, d.expanded);
    assert.match(d.definition, /createdAt: Date/, 'the as-written form keeps the declared type');
  });

  it('a GraphQL request is read by the server as declared', () => {
    const d = resolve({ protocol: 'graphql', type_kind: 'request', side: 'consumer' });
    assert.match(d.expanded, /createdAt: Date/, d.expanded);
  });

  it('an http producer request is the receiving side and keeps its declared Date', () => {
    const d = resolve({ protocol: 'http', type_kind: 'request', side: 'producer' });
    assert.match(d.expanded, /createdAt: Date/, d.expanded);
  });

  it('a class with toJSON() prints what it returns, and bigint stays bigint', () => {
    const wire = resolve(
      { protocol: 'http', type_kind: 'response', side: 'producer' },
      'Endpoint_priced',
    );
    assert.match(wire.expanded, /price: \{ v: number; \}/, wire.expanded);
    assert.match(wire.expanded, /size: bigint/, wire.expanded);
    assert.doesNotMatch(wire.expanded, /toJSON|cents/, wire.expanded);

    const declared = resolve(
      { protocol: 'socket', type_kind: 'response', side: 'producer' },
      'Endpoint_priced',
    );
    assert.match(declared.expanded, /toJSON/, declared.expanded);
  });
});
