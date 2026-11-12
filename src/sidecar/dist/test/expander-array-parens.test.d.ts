/**
 * Regression for the array-element parenthesisation in the shared structural
 * expander (#257 Copilot review). A union/intersection array element must be
 * wrapped in parens before `[]` — `(A | B)[]`, not `A | B[]` — decided from the
 * TYPE, not by string inspection (a union led by an object literal,
 * `{ a: string } | null`, must still be parenthesised; a plain object array
 * `{ a: string }[]` must NOT).
 */
export {};
