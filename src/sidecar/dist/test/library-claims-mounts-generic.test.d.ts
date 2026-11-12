/**
 * carrick#2652: a scope's `mounts` slot whose parameter is typed by the
 * member's own type parameters (`child: Tree<SubEnv, SubBase>`). The receiver
 * is built at its defaults, so the parameter is read with the signature's own
 * type parameters erased to their constraints: the receiver fits when each of
 * its type arguments fits the constraint the parameter puts there.
 *
 * The package is invented, the general shape of a generic router class.
 */
export {};
