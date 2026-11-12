/**
 * carrick#3046 (amendment 7, part 1): a type receiver names its type by a
 * path. `type:A.B` is a type the export's namespace declares, and
 * `type:<module>#A.B` one another module (a types package) declares. The
 * verifier reads the type from its probe line's own type reference, so a
 * generic with no default is read at its declared type (ruling R2); a type's
 * key is the set of declarations it stands on, bases included; `bound:
 * maker|scope` on a type receiver holds when a verified maker returns the
 * same declarations; and `yields.package` names the package that declares
 * the handed type.
 *
 * Every package below is invented and its declarations are hand-written.
 */
export {};
