/**
 * carrick#2091: the check installs a transitive dependency at the version the
 * scanned repo installed, not the newest version in the declared range.
 *
 * A pinned package declares its dependency with a caret range. The registry
 * lists a newer version in that range than the one the repo installed. The
 * producer stub records the installed edge in carrick-resolution.json; the
 * check must install that version.
 *
 * Offline: a local registry on 127.0.0.1, the real vendored pnpm, a scratch
 * store and cache per case, and the real tsc.
 */
export {};
