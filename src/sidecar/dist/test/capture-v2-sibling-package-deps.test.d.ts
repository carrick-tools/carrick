/**
 * A sibling package's declarations that import a dependency only the sibling
 * installs still self-check clean.
 *
 * The service reads a sibling package's SOURCE, so the sibling's declarations
 * land in the stub's tree. The sibling imports a package that sits in the
 * SIBLING's node_modules and not in the service's. The self-check linked only
 * the service's node_modules, so the alias failed as an unresolved pinned
 * external and the response read unverifiable, though the checkout resolves
 * the import from the file's own location.
 */
export {};
