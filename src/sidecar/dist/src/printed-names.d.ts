/**
 * carrick#1836: what each bare name in an inference's printed text meant.
 *
 * The structural printer (`type-structural-expander.ts`) hands some subtrees
 * back to the compiler's own print with no enclosing declaration, and that
 * print writes every named type by its bare name: a database client's row,
 * which is a library's mapped type, prints `{ status: EntityStatus; ... }`.
 * The file a request names often never imports `EntityStatus`, so the text
 * names nothing where it is read (TS2304), and the member reads `any` in the
 * capture's surface.
 *
 * The compiler knew the symbol when it printed the name, and only then. So
 * every type print made while an inference runs is noted (`notePrintedType`),
 * and when it ends (`PrintedTypes.namesIn`) the names its text prints that the
 * request's file cannot resolve are looked up in those prints: the node
 * builder gives each name it writes the symbol it wrote it for. Each such
 * symbol is recorded as the module that declares it and its export path
 * there. A name printed for two declarations is recorded twice, and the
 * reader of the record decides what an ambiguous name means.
 *
 * Only the prints whose text writes such a name are read back, and only when
 * the inference's text has one, so an inference whose names all resolve costs
 * no more than keeping the list.
 */
import { ts, type Node, type Type } from 'ts-morph';
import type { PrintedName } from './types.js';
/**
 * Note a type the compiler printed as `text`, so the inference running now can
 * tell what the names in that print meant. Does nothing outside a recording.
 */
export declare function notePrintedType(type: Type, enclosing: Node | undefined, text: string): void;
/** The type prints one inference made. */
export declare class PrintedTypes {
    private readonly prints;
    /** Run `read`, noting every type it prints. */
    during<T>(read: () => T): T;
    /**
     * The declarations behind each name `texts` print that `source` does not
     * resolve, sorted, one entry per distinct declaration. `checker` must be
     * the one the prints were made with: call this before the program changes.
     */
    namesIn(texts: readonly string[], source: ts.SourceFile, checker: ts.TypeChecker): PrintedName[];
}
