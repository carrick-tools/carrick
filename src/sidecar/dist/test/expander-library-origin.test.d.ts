/**
 * carrick#1264 (third gate): the structural printer must decide library origin
 * from the PROGRAM, not from a `node_modules` path segment.
 *
 * A runtime that serves an installed package's types out of its own cache
 * resolves them to a path with no `node_modules` anywhere in it, and hands the
 * compiler the graph's `isExternalLibraryImport` verdict instead. A printer
 * that tests the path recognises nothing there and walks the package's
 * internals as if they were the user's own contract. What that costs is not
 * verbosity: an interface declared as `extends Array<T>` is an object type
 * with the whole array prototype on it, whose signatures carry `thisArg?: any`
 * — so the printed contract carries a top type that is not in the contract,
 * and the row is demoted to unresolved.
 *
 * The fixture is a real temp tree because both halves of the mechanism are the
 * compiler's: the resolver's verdict and the prototype members it materialises.
 */
export {};
