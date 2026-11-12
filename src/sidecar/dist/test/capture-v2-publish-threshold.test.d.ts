/**
 * What the capture record must say for the scanner to decide whether an
 * alias's answer is worth publishing (carrick#1165).
 *
 * The scanner publishes the capture's printed answer as an operation's type
 * and counts the operation typed. Two answers print a concrete-looking shape
 * that names something the stub tree cannot resolve:
 *
 *  - literal text (printed upstream) or an anonymous print reusing a source
 *    annotation whose import did not resolve, naming an identifier nothing
 *    declares;
 *  - an emitted declaration whose own module imports a module that does not
 *    exist on the scanned checkout.
 *
 * Both used to reach the record only as prose (or not at all), so nothing
 * downstream could refuse them. The record now carries them as data:
 * `undeclared_names` and `dangling_specifiers`.
 *
 * carrick#1377 then narrowed the first of those to what it could not repair.
 * A name the print reaches is rewritten to `unknown` at its own member
 * position, so the rest of the shape is published and the member is labelled
 * `unresolved_import`; `undeclared_names` describes the PRINTED answer, and
 * the printed answer no longer names it. The field stays for what the rewrite
 * cannot reach and for an artifact an older scanner wrote, both of which the
 * publish gate must still refuse. `dangling_specifiers` — a name inside an
 * EMITTED declaration rather than a printed one — is untouched.
 */
export {};
