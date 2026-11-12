/**
 * carrick#1775: an infer anchor whose span covers exactly a TYPE node serves
 * that type, never the declaration around it.
 *
 * The scanner points an `Expression` request at the property type a generated
 * GraphQL document declaration states for a root field (carrick#1761), so the
 * span names a type node inside a type alias, not a value. The capture's span
 * locator accepted value-space nodes only: no node covered the span, the
 * locator fell back to the first "expression" on the line, which on a one-line
 * alias is the alias's own NAME, and the alias served the whole operation
 * result under a field-keyed row.
 */
export {};
