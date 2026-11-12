/**
 * A capture leaves the repo it scans exactly as it found it (carrick#1748).
 *
 * Each fixture drives the write paths a real capture takes: the surface entry
 * placed inside the repo's rootDir, the stub's `node_modules` link into the
 * repo, the paths rewrite, the dangling-import repair (reaching a workspace
 * package's TypeScript source, the carrick#1742 shape), and for Deno the
 * graph root and the runtime cache. Afterwards every file, directory and link
 * in the repo is byte-identical. The one place a scan may write inside a repo
 * is a Deno root's documented cache, `.carrick/deno`, and only there.
 */
export {};
