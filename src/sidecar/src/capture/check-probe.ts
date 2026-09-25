/**
 * Probe generation for the v2 check phase ("tsc as the judge").
 *
 * One probe file per matched pair. The probe is a value-level assignment in
 * the data-flow direction, guarded by six top/bottom-type gates on BOTH sides
 * (IsAny/IsUnknown/IsNever). Value-level (not `[X] extends [Y]`) because the
 * conditional-type relation diverges around `any`, and because the compiler's
 * elaborated assignment error is the user-facing mismatch report.
 *
 * An `http` pair carries a SECOND assignment of the same value in the form JSON
 * puts on the wire, and that one decides the bucket (see `wireAssignmentLine`):
 * a payload is serialised before it travels, so the declared types are not what
 * meet each other. Both questions go to the same judge; nothing here decides a
 * verdict.
 *
 * GraphQL pairs additionally unwrap the producer's resolver-return ENVELOPE
 * before the assignment (see the `graphql` branch in `buildProbe`): a GraphQL
 * producer's captured type is the resolver function's return type with
 * transport layers (Promise / async-iterator) already peeled at inference
 * time, which for wrapper-returning resolvers is a single-payload envelope
 * (`{ data: Order; errors: string[] }`) — not the SDL field payload the
 * consumer's selection set reads. The v1 checker unwrapped this structurally
 * at compare time (ts_check type-checker `unwrapGraphqlPayload`, deleted in
 * WP8); the type-level port here is its v2-native equivalent.
 *
 * Seam: node builtins + `typescript` + this bundle only. No imports needed here.
 */

import type {
  CheckPairEndpoint,
  CheckPairSpec,
  ProbeProtocol,
  ProbeTypeKind,
} from './api.js';

/** Which pair endpoint supplies the `sent` value / the `expected` binding. */
export type Side = 'producer' | 'consumer';

export interface Direction {
  sent: Side;
  expected: Side;
}

/**
 * The direction table, keyed on (protocol, type_kind) — one place. Structurally
 * fixes the confirmed HTTP request-body inversion (data flows consumer ->
 * producer for request bodies, so the check is consumer <= producer).
 *
 *  | protocol       | type_kind | sent     | expected |
 *  | http, graphql  | response  | producer | consumer |
 *  | http           | request   | consumer | producer |
 *  | socket, pubsub | both      | consumer | producer |
 */
export function directionFor(
  protocol: ProbeProtocol,
  typeKind: ProbeTypeKind
): Direction {
  if (protocol === 'socket' || protocol === 'pubsub') {
    return { sent: 'consumer', expected: 'producer' };
  }
  if (protocol === 'http' && typeKind === 'request') {
    return { sent: 'consumer', expected: 'producer' };
  }
  // http/graphql response, and any other (http, graphql) shape.
  return { sent: 'producer', expected: 'consumer' };
}

/** Deterministic FNV-1a (32-bit) over the pair's stable key. Never a path. */
export function fnv1a(input: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    // hash *= 16777619, kept in 32-bit unsigned range.
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, '0');
}

/** The pair ID is derived from the pair's semantic identity, not the workspace
 * path, so it is byte-stable across runs. */
export function pairId(spec: CheckPairSpec): string {
  const key = [
    spec.protocol,
    spec.type_kind,
    spec.producer.service_name,
    spec.producer.alias,
    spec.consumer.service_name,
    spec.consumer.alias,
    spec.pair_key,
  ].join('|');
  return fnv1a(key);
}

/** Names each gate line so a TS2344 can be attributed to a specific side+kind. */
export type GateName =
  | 'sent:any'
  | 'sent:unknown'
  | 'sent:never'
  | 'sent:void'
  | 'sent:form'
  | 'expected:any'
  | 'expected:unknown'
  | 'expected:never'
  | 'expected:void';

export interface ProbePlan {
  pairId: string;
  spec: CheckPairSpec;
  /** Probe file basename, e.g. pair_1a2b3c4d.ts */
  fileName: string;
  /** Which endpoint is sent vs expected (resolved via the direction table). */
  direction: Direction;
  sentEndpoint: CheckPairEndpoint;
  expectedEndpoint: CheckPairEndpoint;
  /** Generated probe source. */
  source: string;
  /** Lines the surface imports sit on (1-based). Errors here => unverifiable. */
  importLines: number[];
  /** 1-based line -> gate name. TS2344 here => baked-any / unverifiable. */
  gateLines: Map<number, GateName>;
  /** 1-based line of the value-level assignment of the DECLARED sent type. */
  assignmentLine: number;
  /**
   * 1-based line of the second assignment, which sends the same value in the
   * form JSON puts on the wire (carrick-tools/carrick-cloud#1119). Present for
   * `http` pairs, whose payload is serialised; absent for every other
   * protocol, where the declared form is what travels.
   *
   * This is the DECISIVE line when present: the comparand short-circuits to
   * the declared type whenever that already assigns, so an error here means
   * the shapes disagree in both forms, and no error here means they agree in
   * the form that actually travels.
   */
  wireAssignmentLine?: number;
}

/** The assignment line whose diagnostic decides the bucket. */
/**
 * The JSON wire transform as TypeScript declarations: `JsonWire<T>` is the
 * form `JSON.stringify` puts `T` on the wire in (a value with `toJSON()`
 * travels as what it returns), and `JsonWireSame<A, B>` says two forms are
 * mutually assignable. `prefix` namespaces the three names so the same
 * transform can be appended to a file that is not ours (the retype check,
 * carrick#1491) without colliding with anything the file declares. The probe
 * uses no prefix.
 *
 * `null` and `undefined` pass through before any other test. Without
 * `strictNullChecks` (the retype runs under the consumer's own options) both
 * are assignable to every type, so the `toJSON` test held for them and
 * `infer R` came back `unknown` (carrick#1514).
 */
export function jsonWireDeclarations(prefix: string): string[] {
  const depth = `${prefix}JsonWireDepth`;
  const wire = `${prefix}JsonWire`;
  const same = `${prefix}JsonWireSame`;
  return [
    `type ${depth} = [never, 0, 1, 2, 3, 4, 5, 6];`,
    `type ${wire}<T, D extends number = 6> = [D] extends [never] ? T : T extends null | undefined ? T : T extends { toJSON: (...args: any[]) => infer R } ? ${wire}<R, ${depth}[D]> : T extends (...args: any[]) => any ? T : T extends object ? { [K in keyof T]: ${wire}<T[K], ${depth}[D]> } : T;`,
    `type ${same}<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;`,
  ];
}

export function decisiveAssignmentLine(plan: ProbePlan): number {
  return plan.wireAssignmentLine ?? plan.assignmentLine;
}

/**
 * Build one probe, recording the exact line of every gate and the assignment so
 * the classifier never depends on hard-coded offsets. `packageOf` maps a
 * service name to its workspace package specifier (e.g. `@carrick/orders`).
 */
export function buildProbe(
  spec: CheckPairSpec,
  packageOf: (serviceName: string) => string
): ProbePlan {
  const id = pairId(spec);
  const direction = directionFor(spec.protocol, spec.type_kind);
  const sentEndpoint = direction.sent === 'producer' ? spec.producer : spec.consumer;
  const expectedEndpoint =
    direction.expected === 'producer' ? spec.producer : spec.consumer;

  const sentPkg = packageOf(sentEndpoint.service_name);
  const expectedPkg = packageOf(expectedEndpoint.service_name);

  const lines: string[] = [];
  const importLines: number[] = [];
  const gateLines = new Map<number, GateName>();

  const push = (text: string): number => {
    lines.push(text);
    return lines.length; // 1-based line number just written
  };

  importLines.push(
    push(`import type { ${sentEndpoint.alias} as Sent } from '${sentPkg}';`)
  );
  importLines.push(
    push(`import type { ${expectedEndpoint.alias} as Expected } from '${expectedPkg}';`)
  );
  push(`type IsAny<T> = 0 extends 1 & T ? true : false;`);
  push(`type IsUnknown<T> = unknown extends T ? (0 extends 1 & T ? false : true) : false;`);
  push(`type IsNever<T> = [T] extends [never] ? true : false;`);
  push(`type Not<T extends boolean> = T extends true ? false : true;`);
  push(`type Assert<T extends true> = T;`);
  gateLines.set(push(`type _G_sent_any = Assert<Not<IsAny<Sent>>>;`), 'sent:any');
  gateLines.set(push(`type _G_sent_unknown = Assert<Not<IsUnknown<Sent>>>;`), 'sent:unknown');
  gateLines.set(push(`type _G_sent_never = Assert<Not<IsNever<Sent>>>;`), 'sent:never');
  gateLines.set(push(`type _G_expected_any = Assert<Not<IsAny<Expected>>>;`), 'expected:any');
  gateLines.set(
    push(`type _G_expected_unknown = Assert<Not<IsUnknown<Expected>>>;`),
    'expected:unknown'
  );
  gateLines.set(push(`type _G_expected_never = Assert<Not<IsNever<Expected>>>;`), 'expected:never');
  // carrick#1162: `void`/`undefined` is what a call site that reads no body
  // types its response as (a wrapper returning `Promise<void>`). It states no
  // contract, so no counterparty shape can be judged against it.
  // `any` is excluded first: `[any] extends [void]` holds, and an `any` side
  // must keep its own top-type reason, never read as "reads no body".
  push(
    `type IsVoid<T> = 0 extends 1 & T ? false : [T] extends [never] ? false : [T] extends [void] ? true : false;`
  );
  gateLines.set(push(`type _G_sent_void = Assert<Not<IsVoid<Sent>>>;`), 'sent:void');
  gateLines.set(push(`type _G_expected_void = Assert<Not<IsVoid<Expected>>>;`), 'expected:void');
  // carrick#1162: a form-encoded request body (the platform `FormData` or
  // `URLSearchParams`) carries its fields as runtime appends, which no type
  // records, so a field-by-field comparison against a declared shape reads
  // every such body as missing all of its fields.
  if (spec.protocol === 'http' && spec.type_kind === 'request') {
    push(
      `type IsFormBody<T> = 0 extends 1 & T ? false : [T] extends [never] ? false : [T] extends [FormData | URLSearchParams] ? true : false;`
    );
    gateLines.set(push(`type _G_sent_form = Assert<Not<IsFormBody<Sent>>>;`), 'sent:form');
  }
  push(`declare const sent: Sent;`);

  let assignmentLine: number;
  if (spec.protocol === 'graphql') {
    // GraphQL selection-set semantics (v1 `unwrapGraphqlPayload` parity): the
    // producer side (always `sent` for graphql — see the direction table) is
    // the resolver's return type. When that return is a single-payload
    // ENVELOPE — exactly one property whose type is payload-shaped (an
    // object, an array of objects, or a union of objects), every sibling a
    // scalar or scalar array (`errors: string[]`) — the consumer's selection
    // reads the payload, not the envelope, so the payload is the comparand.
    //
    // Fail-closed by construction:
    //   - `Sent` already assignable to `Expected` short-circuits to `Sent`
    //     (plain structural subset selection stays exactly as before);
    //   - zero payload-shaped properties (already-bare payloads of scalars)
    //     or several (ambiguous envelope, or a bare payload whose own fields
    //     include more than one object-shaped property) keep the whole type —
    //     the unwrap never fires on anything but an unambiguous envelope;
    //   - `GqlObjLike` can never select `any`/`unknown`/`never` (top/bottom
    //     types fail both its array and its object arm), and the comparand is
    //     re-gated below anyway — the v2 port of v1's comparand re-guard, so
    //     a payload decayed to a top type can never read compatible.
    push(
      `type GqlObjLike<V> = [V] extends [readonly unknown[]] ? ([V] extends [readonly (infer E)[]] ? ([E] extends [readonly unknown[]] ? false : ([E] extends [object] ? true : false)) : false) : ([V] extends [object] ? true : false);`
    );
    push(
      `type GqlIsUnion<T, U = T> = [T extends unknown ? ([U] extends [T] ? false : true) : never] extends [false] ? false : true;`
    );
    push(
      `type GqlPayloadKeys<T> = { [K in keyof T]-?: [GqlObjLike<T[K]>] extends [true] ? K : never }[keyof T];`
    );
    push(
      `type GqlSingleKey<K> = [K] extends [never] ? never : ([GqlIsUnion<K>] extends [false] ? K : never);`
    );
    push(
      `type GqlPayloadOf<S> = [S] extends [readonly unknown[]] ? never : [GqlIsUnion<S>] extends [true] ? never : [S] extends [object] ? ([GqlSingleKey<GqlPayloadKeys<S>>] extends [never] ? never : S[GqlSingleKey<GqlPayloadKeys<S>> & keyof S]) : never;`
    );
    push(
      `type GqlComparand = [Sent] extends [Expected] ? Sent : ([GqlPayloadOf<Sent>] extends [never] ? Sent : GqlPayloadOf<Sent>);`
    );
    gateLines.set(
      push(`type _G_comparand_any = Assert<Not<IsAny<GqlComparand>>>;`),
      'sent:any'
    );
    gateLines.set(
      push(`type _G_comparand_unknown = Assert<Not<IsUnknown<GqlComparand>>>;`),
      'sent:unknown'
    );
    gateLines.set(
      push(`type _G_comparand_never = Assert<Not<IsNever<GqlComparand>>>;`),
      'sent:never'
    );
    push(`declare const sentComparand: GqlComparand;`);
    assignmentLine = push(`const expected: Expected = sentComparand;`);
  } else {
    assignmentLine = push(`const expected: Expected = sent;`);
  }

  // The JSON wire line (carrick-tools/carrick-cloud#1119). An `http` payload is
  // serialised before it travels, and `JSON.stringify` writes a value's
  // `toJSON()` RESULT: a producer's `Date` arrives at the consumer as the
  // string it serialises to, so comparing the DECLARED `Date` against a
  // correctly-declared `string` reports a drift that cannot happen. The
  // transform is applied to the SENT side in BOTH directions, which is where
  // serialisation happens: a consumer that sends a `Date` in a request body
  // likewise delivers a string, so a producer declaring `Date` there is a real
  // mismatch and stays one.
  //
  // `WireSent` short-circuits to the declared type when that already assigns,
  // so a pair that agrees as declared never instantiates the mapped type (no
  // cost, and no way for the transform to turn an agreeing pair into a
  // disagreeing one). tsc stays the judge of both forms.
  let wireAssignmentLine: number | undefined;
  if (spec.protocol === 'http') {
    // Keep the DECLARED type whenever serialising changes nothing observable,
    // so the compiler's headline still names the real surface alias (the probe
    // prints `Sent`, which the scrub rewrites) instead of expanding a mapped
    // type structurally. Only a pair whose payload really is transformed loses
    // that name — and there the declared name no longer describes what travels.
    for (const line of jsonWireDeclarations('')) push(line);
    push(
      `type WireSent = [Sent] extends [Expected] ? Sent : (JsonWireSame<JsonWire<Sent>, Sent> extends true ? Sent : JsonWire<Sent>);`
    );
    push(`declare const sentWire: WireSent;`);
    wireAssignmentLine = push(`const expectedWire: Expected = sentWire;`);
  }

  return {
    pairId: id,
    spec,
    fileName: `pair_${id}.ts`,
    direction,
    sentEndpoint,
    expectedEndpoint,
    source: lines.join('\n') + '\n',
    importLines,
    gateLines,
    assignmentLine,
    wireAssignmentLine,
  };
}
