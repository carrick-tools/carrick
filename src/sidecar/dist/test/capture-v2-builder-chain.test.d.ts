/**
 * #439 part 1: builder-chain producer anchors must select the schema
 * argument, not the config descriptor.
 *
 * A producer request anchor whose line-based locator lands inside a fluent
 * builder chain's config-descriptor argument (`.meta({ openapi: {...} })`)
 * would otherwise capture that all-literal metadata object as the request
 * type. The fix re-aims at the chain's lone schema argument, or demotes when
 * the payload cannot be picked unambiguously — never keeping the descriptor
 * (the artifact behind v1's false-incompatible verdicts).
 *
 * Detection is STRUCTURAL (no method names): the descriptor is a non-empty,
 * all-literal object literal argument of a chain of >=2 fluent calls; the
 * payload is the lone non-literal, non-inline-function chain argument. All
 * fixtures are synthetic and generically named.
 *
 * #497 extends the same suite to the descriptor written as a REFERENCE
 * (`.meta(createWidgetMeta)`, the const declared in a sibling module), which
 * has no object literal ancestor for the walk-up to find, plus the two
 * over-trigger controls that keep the widened trigger from firing on an
 * ordinary schema argument.
 */
export {};
