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
 * GraphQL pairs also read the consumer's `__typename` as optional, at any depth
 * (carrick#1759): the server adds that meta-field to every object a selection
 * asks for, so a resolver's return type never states it, while a consumer type
 * generated from a document that selects it declares it required.
 *
 * Seam: node builtins + `typescript` + this bundle only. No imports needed here.
 */
import type { CheckPairEndpoint, CheckPairSpec, ProbeProtocol, ProbeTypeKind } from './api.js';
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
export declare function directionFor(protocol: ProbeProtocol, typeKind: ProbeTypeKind): Direction;
/** Deterministic FNV-1a (32-bit) over the pair's stable key. Never a path. */
export declare function fnv1a(input: string): string;
/** The pair ID is derived from the pair's semantic identity, not the workspace
 * path, so it is byte-stable across runs. */
export declare function pairId(spec: CheckPairSpec): string;
/** Names each gate line so a TS2344 can be attributed to a specific side+kind. */
export type GateName = 'sent:any' | 'sent:unknown' | 'sent:never' | 'sent:void' | 'sent:form' | 'sent:bytes' | 'expected:any' | 'expected:unknown' | 'expected:never' | 'expected:void' | 'expected:bytes' | 'sent:envelope' | 'expected:envelope';
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
    /**
     * 1-based line where the second reading of the sent side starts
     * (carrick#1538): the import of the producer's UNWIDENED alias, its
     * top-type gates and one more assignment, all after every other line, so no
     * line above it moves. Present only when the caller named that alias.
     *
     * Nothing from this line on is an import line, a gate line or an assignment
     * line of the plan. The classifier reads these lines for one question only:
     * whether a mismatch on the published type holds for the type the handler
     * sends with no literal widened.
     */
    unwidenedFromLine?: number;
}
/**
 * The surface export that carries a response alias's unwidened reading
 * (carrick#1516). The scanner names it when it asks the capture for one
 * (`unwidened_alias` in `src/engine/type_compat_v2.rs`); this is the same
 * name, read back.
 */
export declare function unwidenedAliasOf(alias: string): string;
/** The local name the probe imports the unwidened alias under. */
export declare const UNWIDENED_LOCAL_NAME = "SentUnwidened";
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
 *
 * The depth counts `toJSON()` hops only, never nesting (carrick#2058). A body
 * is mapped at every depth it has, a list's element at the list's own depth,
 * so a `Date` three lists down travels as a string like one at the top. The
 * mapped type is deferred, so a recursive type is expanded only as far as the
 * compiler's own comparison walks it. What the count bounds is a `toJSON()`
 * that returns a value with another `toJSON()`, which a type can do forever
 * (one that returns its own type), and which no depth of nesting reaches.
 */
export declare function jsonWireDeclarations(prefix: string): string[];
export declare function decisiveAssignmentLine(plan: ProbePlan): number;
/**
 * Build one probe, recording the exact line of every gate and the assignment so
 * the classifier never depends on hard-coded offsets. `packageOf` maps a
 * service name to its workspace package specifier (e.g. `@carrick/orders`).
 *
 * `unwidenedAliasFor` answers, for the sending side, the surface export that
 * carries its unwidened reading, when its stub ships one: a producer's
 * response (carrick#1538) or a consumer's request body (carrick#2011). The
 * probe of an `http` pair then ends with a second assignment of that reading,
 * in the wire form. Without an answer the probe is what it always was, byte
 * for byte.
 *
 * `expectedToleratesAbsence` answers, for the receiving side, whether its
 * capture record marks a handler that works when no body is sent
 * (carrick#2814).
 */
export declare function buildProbe(spec: CheckPairSpec, packageOf: (serviceName: string) => string, unwidenedAliasFor?: (sent: CheckPairEndpoint) => string | undefined, expectedToleratesAbsence?: (expected: CheckPairEndpoint) => boolean): ProbePlan;
