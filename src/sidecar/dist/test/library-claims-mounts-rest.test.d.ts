/**
 * carrick#2661: a scope's `mounts` argument that lands on a rest parameter
 * (`use(path, ...handlers)`) is read against the rest's element type.
 *
 * The package is invented, the general shape of a server whose mount member
 * takes its routers as a rest of handlers.
 */
export {};
