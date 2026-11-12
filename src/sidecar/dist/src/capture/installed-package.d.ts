/**
 * Map an absolute path into an installed package onto the specifier a
 * consumer of that package would write (carrick#1174).
 *
 * The v1 printer and the node builder name an out-of-scope type by the
 * absolute path of its declaration file when no bare specifier reaches it
 * from where they print. Inside a capture stub that path is both private (the
 * checkout root, or the home directory of a runtime's npm cache) and dead:
 * the check workspace resolves packages from the stub's pinned dependencies,
 * never from the scanning machine's disk.
 *
 * The replacement is decided against the package itself, installed alone at
 * `node_modules/<name>`, which is exactly what the check workspace sees:
 *  1. an `exports` entry (or, without `exports`, the types entry or the file's
 *     subpath) that resolves to the same file under bundler resolution;
 *  2. failing that, an exported entry whose module exports the imported name
 *     from that same file (a type in an unexported file, re-exported by the
 *     package root);
 *  3. failing both, the subpath as written, so the self-check reports the
 *     specifier as unresolvable rather than the stub leaking the path.
 *
 * Seam: node builtins, `typescript`, and this bundle only.
 */
import ts from 'typescript';
export interface InstalledPackageSpecifier {
    /** Bare specifier: public package name plus subpath, e.g. `pkg/sub`. */
    specifier: string;
    /** The package's own name and real install directory (never written out). */
    install: {
        name: string;
        root: string;
    };
    /** The package the path landed in and its installed version, when published. */
    pin?: {
        name: string;
        version: string;
    };
    /**
     * The name the entry exports the declaration under, set only when it differs
     * from the name the path's declaration carries (`export { O as OmitKeys }`).
     */
    exportName?: string;
}
/**
 * The `@types/*` package that serves a bare name's declarations, for callers
 * deciding whether a runtime name is covered by a pinned types package.
 */
export declare function typesPackageOf(name: string): string;
export declare function toPosix(p: string): string;
/** A path or specifier without its script, source or declaration extension. */
export declare function withoutExtension(subpath: string): string;
/**
 * A compiler host that also resolves the packages an absolute specifier was
 * rewritten into, each from its own install directory. The capture's
 * self-check compiles the stub against the producer's `node_modules`, which
 * does not name a transitive package at its root; the check workspace does,
 * because the stub pins it. Without this the self-check would fail a
 * specifier the check resolves.
 */
export declare function withInstalledPackages(options: ts.CompilerOptions, installs: Record<string, string>, base?: ts.CompilerHost): ts.CompilerHost;
/**
 * The bare specifier for an absolute path into an installed package, or
 * undefined when the path does not land in one. `importedName` is the first
 * name read off the import (`import("...").Name`), when the text has one.
 */
export declare function installedPackageSpecifier(absoluteSpecifier: string, importedName?: string): InstalledPackageSpecifier | undefined;
