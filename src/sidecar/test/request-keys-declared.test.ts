import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import type { SidecarRequest } from '../src/types.js';
import type { ValidatedSidecarRequest } from '../src/validators.js';

/*
 * The schema and the request types declare the same keys (carrick#1980).
 *
 * A request is written twice: as the type its handler reads (`types.ts`,
 * `capture/api.ts`) and as the schema above, which drops every key it does
 * not declare. `parseRequest` casts one to the other, and a schema that lacks
 * an optional key is still assignable to the type that has it, so the
 * compiler never compared them: the capture read a `param_name` off its
 * anchor, and no request that went through the schema carried one.
 *
 * The two are compared here, key by key at every depth, when the sidecar is
 * built. A key on one side only fails the build and is named in the error:
 * `".anchors[].param_name"` does not satisfy the constraint `never`. A
 * request is told from the others by its `action`, an anchor or a claim by
 * its `kind`, and each is compared with the schema of the same one.
 *
 * The comparison is type-level, so it is checked when the sidecar is built
 * (`tsc` compiles `test/`). It lives here, not in `validators.ts`, because
 * that file is vendored and compiled alone elsewhere, where `./types.js`
 * does not resolve (carrick-cloud#2201).
 */

/** Every key any member of a union declares. */
type KeysOfAny<Union> = Union extends unknown ? keyof Union : never;

/** What `Key` holds, across the members of a union that declare it. */
type ValueAt<Union, Key extends PropertyKey> = Union extends unknown
  ? Key extends keyof Union
    ? Union[Key]
    : never
  : never;

/** The members of `Other` with the tag `One` has, when `One` has one. */
type SameVariant<One, Other> = One extends { action: infer Tag }
  ? Extract<Other, { action: Tag }>
  : One extends { kind: infer Tag }
    ? Extract<Other, { kind: Tag }>
    : Other;

/**
 * The keys of `One` that `Other` does not declare, each as the path that
 * reaches it; `never` when there are none. A record (a string index
 * signature) declares no keys: its keys are data, and a recursive one (the
 * GraphQL leaf tree, carrick#4403) would never end.
 */
type UndeclaredKeys<One, Other, At extends string = ''> = One extends readonly (infer Item)[]
  ? UndeclaredKeys<Item, Other extends readonly (infer Element)[] ? Element : never, `${At}[]`>
  : One extends object
    ? string extends keyof One
      ? never
      : {
          [Key in keyof One & string]-?: Key extends KeysOfAny<SameVariant<One, Other>>
            ? UndeclaredKeys<One[Key], ValueAt<SameVariant<One, Other>, Key>, `${At}.${Key}`>
            : `${At}.${Key}`;
        }[keyof One & string]
    : never;

/** Compiles for `never` alone. */
type None<Paths extends never> = Paths;

/** A key a handler may read that the schema would drop. */
type KeysTheSchemaDrops = None<UndeclaredKeys<SidecarRequest, ValidatedSidecarRequest>>;

/** A key the schema lets through that no handler's type knows. */
type KeysNoHandlerKnows = None<UndeclaredKeys<ValidatedSidecarRequest, SidecarRequest>>;

// Referenced so neither alias reads as unused; each is `never` or the build fails.
const keysTheSchemaDrops: KeysTheSchemaDrops | undefined = undefined;
const keysNoHandlerKnows: KeysNoHandlerKnows | undefined = undefined;

describe('request schema and request types declare the same keys', () => {
  it('compiles only when neither side has a key the other lacks', () => {
    assert.equal(keysTheSchemaDrops, undefined);
    assert.equal(keysNoHandlerKnows, undefined);
  });
});
