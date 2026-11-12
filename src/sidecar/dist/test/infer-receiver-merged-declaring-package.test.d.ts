/**
 * carrick#2031: a receiver whose type is a global interface that several
 * declaration files merge (the compiler's default library declares it, a
 * typings package augments it) is attributed to one package, whatever the
 * process did before.
 *
 * On TypeScript 6 the merged symbol's declarations come back in another order
 * once the program has been rebuilt (any edit, such as the retype's rewrite
 * or the inferrer's marking pass, rebuilds it): the augmentation's
 * declaration comes first. Taking the first declaration therefore named the
 * typings package after a rebuild and the compiler's library before it. The
 * rule: where the default library declares the interface, the package is the
 * library's; otherwise it is the declaring file whose path sorts first.
 */
export {};
