/**
 * The compiler's own print of a type, with a module the print names written
 * as the file that declares it (carrick#2019).
 *
 * When a type comes from a module the enclosing file does not import, the
 * compiler writes it as `import("<module>").Name`. TypeScript 5 wrote the
 * declaring file's path there: the node builder had no module-specifier host
 * unless a declaration emit gave it one. TypeScript 6 gives every print a
 * host, so the same type printed at an enclosing node reads
 * `import("../model").Name` or `import("<package>").Name`: a specifier that
 * depends on which file it was printed from, so one type printed from two
 * files gives two texts. Everything downstream reads the declaring file's
 * path: the scanner's path scrub writes it repo-relative or as
 * `<package>@<version>`, and a capture pastes an inferred text into its
 * surface entry, where a specifier relative to another file names nothing.
 *
 * These functions run the compiler's own code with no host: a tracker whose
 * `moduleResolverHost` is `false` (falsy, so the builder writes the declaring
 * file; not nullish, so the builder does not supply its own). Nothing in the
 * text is rewritten afterwards. `typeToStringAsDeclared` is the checker's
 * `typeToString`, step for step, with that one tracker.
 *
 * Only prints with an enclosing node differ: with none, the compiler has no
 * file to write a specifier relative to and writes the declaring file
 * anyway. A print that must name modules the way a declaration file would
 * (the capture's surface emit) passes a host of its own and does not use
 * this.
 */
import { ts } from 'ts-morph';
/**
 * The compiler's internal `SymbolTracker`: callbacks the node builder reports
 * to, and the module-specifier host it writes specifiers with.
 */
export interface SymbolTracker {
    moduleResolverHost?: unknown;
}
/**
 * The checker's `typeToTypeNode`, writing each module a node names as the
 * file that declares it. A caller's own tracker keeps its callbacks.
 */
export declare function typeToTypeNodeAsDeclared(checker: ts.TypeChecker, type: ts.Type, enclosing: ts.Node | undefined, flags: ts.NodeBuilderFlags, tracker?: SymbolTracker): ts.TypeNode | undefined;
/**
 * The checker's `typeToString(type, enclosing, flags)`, writing each module
 * the print names as the file that declares it. `noErrorTruncation` is the
 * program's option of that name, which `typeToString` reads when `flags`
 * leave truncation on.
 */
export declare function typeToStringAsDeclared(checker: ts.TypeChecker, type: ts.Type, enclosing: ts.Node | undefined, flags?: ts.TypeFormatFlags, noErrorTruncation?: boolean): string;
