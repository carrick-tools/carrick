/**
 * A declared request schema publishes what a caller SENDS: the schema's input
 * type, not its parsed output (carrick#1101).
 *
 * Every request anchor that reads a validation schema used to read its output
 * (`parse` return, `_output`). A caller sends the input, and the two differ
 * wherever parsing changes the value: a defaulted key is optional to send and
 * required after parsing, and a transform changes the member's type. The
 * request check is consumer-to-producer, so publishing the output turns a
 * caller that correctly omits a defaulted key into a mismatch.
 *
 * Locked in here, across the three request paths (validator middleware, route
 * `schema.body`, and a located validated read on its own line):
 *
 *  1. the input is read from the Standard Schema member
 *     `~standard.types.input` first, then `_input`, and only a schema with
 *     neither publishes its output;
 *  2. a response bound to the same schema keeps the output;
 *  3. a member whose input is `unknown` where the output is concrete (a
 *     coercion accepts any value) publishes its OUTPUT type and is recorded as
 *     `coerced_input` in `any_provenance`, rather than abstaining. The decision
 *     is per member (carrick#1105): every other member keeps its input, so a
 *     defaulted key beside a coerced one stays optional, and the key's own
 *     optionality always comes from the input.
 *
 * The fake `schema-lib` exposes both members, and `standard-lib` exposes only
 * `~standard`, so each read order is exercised on its own.
 */
export {};
