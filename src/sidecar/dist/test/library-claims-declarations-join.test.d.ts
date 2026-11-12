/**
 * carrick#2654: a verified verdict's `declarations` join the position row of
 * the call they judge. A verdict names the declaration the call row names for
 * the call's member symbol: for a member declared as a property and typed by
 * an interface with call signatures, the property's own declaration (not the
 * signatures' lines in the interface); for a method-declared member, the
 * method. Verified ops and verified scopes follow the one rule.
 *
 * The package is invented: a class with property-typed members (`get`,
 * `mount`, each typed by an interface with overloaded call signatures) beside
 * method-declared ones (`post`, `mountOn`).
 */
export {};
