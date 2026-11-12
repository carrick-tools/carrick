/**
 * carrick#1843: a wrapper rule that names a type alias unwraps it.
 *
 * A type goes by up to two names: the symbol of the declaration its structure
 * comes from, and the alias it was written through. The compiler states each
 * with its own argument list. The matcher read the own symbol only, and the
 * payload read the own arguments only, so a rule naming an alias never
 * unwrapped one:
 *
 *  - an alias of a class has the class's symbol, so the rule's name missed;
 *  - an alias of an object literal has the anonymous `__type`, so the name
 *    missed, and the literal has no arguments of its own;
 *  - an alias of a union matched by name, having no own symbol to hide it,
 *    and then read no payload, a union having no arguments of its own.
 *
 * A rule now matches either name, and the payload is the argument stated for
 * the name that matched: `payloadGenericIndex` counts the parameters of the
 * declaration the rule names, which an alias is free to order differently
 * from the type it stands for.
 *
 * What the rules do not decide is left undecided. A named argument that is a
 * top type is no payload, and the next argument along is not read in its
 * place: on a result carrier that is the error side. An alias the rule does
 * not name and its modules do not declare lends no arguments to it.
 *
 * On a result carrier (`Task<Outcome<Reply<T>, E>>`) the rules now reach what
 * the carrier holds before the carrier read does (carrick#1376,
 * carrick#1841). The site answers as that read answered: a payload by its
 * members, anchored on it, and verified transport with no payload as the
 * decided abstain, whether the service has a rule for every layer, for some,
 * or for none of the outer ones.
 */
export {};
