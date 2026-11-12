/**
 * A request library as an installed package, for the tests that read a call's
 * result through a service's wrapper rules (carrick#1841, carrick#1843).
 *
 * Every wrapper it exports is a type alias, in the forms the language has for
 * one:
 *
 *  - `Task<A>`, an alias of a class. The type has the class's own symbol
 *    (`__Task`) and own arguments, and the alias's symbol and arguments
 *    beside them;
 *  - `Outcome<A, E>`, an alias of a union. A union has no symbol and no
 *    arguments of its own, so the alias states both;
 *  - `Reply<T>`, an alias of an object literal. Its own symbol is the
 *    compiler's anonymous `__type`, and its arguments are the alias's;
 *  - `Scoped<Scope, T>`, an alias of an interface whose parameters are not
 *    the interface's own: `Envelope<T>` has one argument, the alias two.
 *
 * `Envelope<T>` is an interface, the form a rule has always read.
 */
export declare const WIRE_PACKAGE = "tiny-wire";
/** The module globs a rule for this package carries. */
export declare const WIRE_ORIGIN: string[];
/**
 * A temp repo with the package installed under `node_modules` and the given
 * sources under `src/`. The caller removes `repoDir`.
 */
export declare function writeWireRepo(prefix: string, sources: Record<string, string>): {
    repoDir: string;
    pathOf: (name: string) => string;
};
/** A sidecar `infer` answer, as far as these tests read it. */
export interface WireInferred {
    alias: string;
    type_string: string;
    is_explicit: boolean;
    primary_type_symbol?: string;
    array_depth?: number;
    raw_text_read?: boolean;
    any_provenance?: Array<{
        path: string;
        kind: string;
        reason: string;
        detail?: string;
    }>;
}
export declare const collapse: (text: string) => string;
