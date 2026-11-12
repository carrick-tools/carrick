/**
 * carrick#2898: the unwidened reading must not depend on which request built
 * the program last. A service imports a workspace package that sits outside
 * its `rootDir` (through a `node_modules` symlink), so the compiler reports
 * TS6059 at the import. That diagnostic belongs to the program, not to the
 * marking; it used to taint the reading on the first batch of a process and
 * not on the next, so a process that had answered before differed from a
 * fresh one.
 */
export {};
