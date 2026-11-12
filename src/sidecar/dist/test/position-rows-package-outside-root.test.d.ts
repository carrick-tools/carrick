/**
 * A declaration in an installed package is named in rows whatever the
 * install tool's layout (carrick#2599).
 *
 * Some resolvers serve a package from a cache outside the scanned repo whose
 * path has no `node_modules` segment (`<cache>/<registry>/<name>/<version>/…`),
 * so reading the package directory off the path finds nothing. The module
 * graph that resolved the file knows the package directory; `PathNames` asks
 * it after the root and the path rule, and names the file
 * `pkg:<name>/<path in the package>`, as it names a package installed above
 * the root.
 */
export {};
