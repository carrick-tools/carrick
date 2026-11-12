/**
 * Blueprint row N8: whether a type is framework machinery is decided by where
 * it is declared, never by its name.
 *
 * - A type declared by the platform's library (`Response`) that carries the
 *   transport's members is machinery (`typeIsFrameworkMachinery`).
 * - A type declared by an installed package the store labels a server
 *   framework (`framework_packages` on the infer request) is machinery,
 *   whatever members it has.
 * - A service's own `Context`, `Request` or `Router` is its contract.
 *
 * `machinery` marks the type `type_string` prints; `anchor_machinery` marks
 * the type `primary_type_symbol` names.
 */
export {};
