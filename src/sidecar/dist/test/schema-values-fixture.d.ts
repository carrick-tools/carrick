/**
 * Two invented packages for the foreign-maker gate (carrick#3745).
 *
 * `fixture-schema-kit` declares a generic builder interface whose every
 * method returns the builder again at the same type argument, plus a chain of
 * wrapper interfaces. `fixture-schema-values` exports many values typed by
 * that builder, each at its own type argument: the checker keeps one copy of
 * the builder's whole surface per value, and listing every member's receiver
 * on all of them runs the lister out of heap. It also exports a hub typed by
 * the kit (a foreign value with one member maker), a client typed by its own
 * package, a function, and the kit's own default builder, re-exported.
 */
/**
 * The estimate the gate computes: per value, `wrappers + 3` member makers
 * (parse, describe, check and one per wrapper) times `wrappers + 4` members
 * (those and `output`), plus 1 for the hub (one maker, one member).
 */
export declare function foreignMakerEstimate(values: number, wrappers: number): number;
/** Member makers a gated listing leaves out: the values' own, plus the hub's. */
export declare function foreignMakersLeftOut(values: number, wrappers: number): number;
export declare function kitDeclarations(wrappers: number): string;
export declare function valuesDeclarations(values: number): string;
/** A probe project in `dir` with both packages installed. */
export declare function writeSchemaValuesProject(dir: string, values: number, wrappers: number): void;
