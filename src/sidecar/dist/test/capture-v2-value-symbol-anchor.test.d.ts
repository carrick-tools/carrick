/**
 * #438 part 1 (value-symbol guard) + #439 part 2 (const->type re-aim).
 *
 * An LLM symbol anchor may name a schema VALUE const
 * (`export const ZFooSchema = object({...})`) instead of its sibling
 * `export type TFoo = Infer<typeof ZFooSchema>`. Emitting the const in TYPE
 * position produces a broken surface line (a value used as a type) that
 * poisons the whole producer service. This suite pins the fix:
 *
 *  A. a value-only export with no type sibling DEMOTES (honest `unknown`, no
 *     broken surface line, reason recorded);
 *  B. a value-only export WITH a single `typeof`-derived sibling type alias
 *     RE-AIMS at that alias and captures the inferred object type;
 *  - a real type / a class (value + type meaning) still emit unchanged;
 *  - two sibling type aliases over one const are ambiguous -> demote (A).
 *
 * All fixtures are synthetic and generically named; the schema helper is a
 * local stand-in for any `object(...)` + `Infer<typeof ...>` library.
 */
export {};
