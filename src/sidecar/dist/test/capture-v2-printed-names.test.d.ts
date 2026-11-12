/**
 * carrick#1836: a literal anchor's bare name that its source file never names
 * resolves through the declaration the inference printed it for.
 *
 * The v1 inferrer prints some types with no enclosing declaration, so a
 * member typed by a database client's enum comes back as
 * `status: EntityStatus` although the route's file never imports the enum.
 * #1774 reads names in the anchor's source file, which cannot help: there the
 * name means nothing, the surface member reads `any`, and the record lists it
 * as unresolved (#1446).
 *
 * The inference now records what each such name meant (`printed_names`), and
 * the capture imports the name from the module that declares it, when that
 * module is inside the repo. A name recorded for two declarations is left as
 * written: the text no longer says which one a position meant.
 *
 * The capture's own self-check is not the oracle here; the test reads the
 * stub's surface with the compiler.
 */
export {};
