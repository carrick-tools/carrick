/**
 * The README's protocol section is checked against the validators, not read
 * (carrick#755).
 *
 * It drifted far enough that a request written from it was rejected — the
 * `infer` example used field names the schema has never had, and six actions
 * were missing entirely. Prose cannot be trusted to keep up with a schema, so
 * this test holds it: every JSON request example must parse, and every action
 * in the discriminated union must appear in the README.
 */
export {};
