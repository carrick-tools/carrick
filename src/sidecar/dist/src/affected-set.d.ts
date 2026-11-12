/**
 * Which files an edit can reach (carrick#2881, carrick#3100).
 *
 * A PR check re-types only the aliases its change can reach. This module says
 * where: the changed files, and every file whose types read a declaration the
 * edit changed, followed name by name rather than file by file, so a barrel
 * (`index.ts` with `export *`) does not turn one changed export into every
 * importer of the package.
 *
 * The walk:
 * - In a changed file, a top-level name is changed when the text of a
 *   statement declaring it changed, or when a statement declaring it mentions
 *   a changed name of the same file (so `type Dto = z.infer<typeof schema>`
 *   changes with `schema`). A statement that declares nothing (an expression
 *   statement, which can add an expando member to a function) changes every
 *   name it mentions when it changes or mentions a changed name. In a
 *   CommonJS file, a changed statement that mentions `module` or `exports`
 *   changes the whole module.
 * - An importing file is reached when one of its imports resolves, through
 *   every re-export (the compiler's own alias resolution), to a changed
 *   declaration. A namespace import reads every export of its module, and so
 *   does a dynamic `import()`, `require()` or `import('m').T`, read as a
 *   binding of the statement that holds it. Its names that
 *   mention the reached imports change in turn, and the walk goes on to their
 *   importers. A name whose declaration mentions no changed name stops the walk
 *   there, whatever its printed type says.
 * - An added or deleted file is an empty module (`export {};`) on its missing
 *   side. It changes every name of the file, and its importers are reached
 *   through the graph. Its export names are not matched against importers
 *   elsewhere in the repo: nothing imports a name from a file it did not
 *   resolve to. What it can change is where an import is looked up, so every
 *   import records the stem it was looked up at (a relative specifier's path,
 *   and the repo file it resolved to, without extension and trailing
 *   `/index`), and an added or deleted file whose own stem matches reaches
 *   that import's file.
 * - A global declaration or a module augmentation (`declare global`, `declare
 *   module 'x'`) is read by files that never import it (a value returned by
 *   the augmented package carries it), so a change to one, or to a name one
 *   mentions, is returned in `augmented`: the caller widens it to every file
 *   of the programs that hold it, as TypeScript's own scope for an
 *   augmentation does. So is any change to a script (a file with no import or
 *   export, all of it global). A file that is not TypeScript or JavaScript
 *   source, or an installed package's file, reaches every file. An import that
 *   resolves to an augmented interface reads the augmentation by its own name
 *   (`AUGMENTATION`), so an unchanged augmentation stops nothing.
 * - An export added or removed in a file that exists on both sides (an export
 *   list, or a new exported declaration) reaches every file importing that
 *   name from anywhere, and the statements of every file holding a namespace
 *   or dynamic import that read it.
 *
 * Dependencies run from a file to the declarations it names; the compiler
 * never types a file from the files that import it, so nothing is followed the
 * other way. One assumption: an installed package's declarations do not import
 * the workspace's own sources (only a workspace package could, and its files
 * are read at their real path, outside `node_modules`).
 *
 * The graph is read from a program's own checker, once per program, and keyed
 * by path relative to a root. `serialiseGraph` writes it without statement
 * texts; `reach` walks that form, so a caller needs no program.
 */
import { ts } from 'ts-morph';
import type { SerialGraph } from './types.js';
/** A top-level declaration a file's import reads: `name` `*` is the whole file. */
export interface Origin {
    file: string;
    name: string;
}
/** The top-level statements of one file, read from its text alone. */
export interface Shape {
    /** Each top-level name, with the texts of the statements declaring it, in order. */
    declared: Map<string, string[]>;
    /** Each top-level name, with the identifiers its statements mention. */
    refs: Map<string, Set<string>>;
    /** Statements that declare nothing: where each starts, its text and the identifiers it mentions. */
    loose: Array<{
        pos: number;
        text: string;
        refs: Set<string>;
    }>;
    /** The names importers can import from it. */
    exports: Set<string>;
    /**
     * Each name an export list or `export default x` gives a local: an importer
     * of the name resolves to the local's declaration, so pointing the name at
     * another local moves it as adding it would.
     */
    exportTargets: Map<string, string>;
    /** It is a script: all of it is global, and a change reaches every file. */
    global: boolean;
}
/** One file of the graph: its shape and where each of its imports leads. */
interface FileNode {
    shape: Shape;
    /** Each import binding, by local name: the declarations it resolves to. */
    bindings: Map<string, Origin[]>;
    /** Each named import binding, by local name: the name it imports. */
    importedNames: Map<string, string>;
    /**
     * The bindings that read every export of a module: a namespace import, an
     * `import x = require()`, and each dynamic `import()`, `require()` or
     * `import('m').T`, bound under a name no identifier can spell and mentioned
     * by the statement that holds it.
     */
    wholeLocals: Set<string>;
    /** Where each import was looked up: an added or deleted file at that stem changes what it resolves to. */
    stems: StemEntry[];
}
/** One import specifier of a file, and the stems it was looked up at. */
export interface StemEntry {
    /** The stems: the relative specifier's path, and the repo file it resolved to. */
    stems: string[];
    /** The repo file it resolved to, when it resolved to one. */
    target?: string;
    /** The bindings the specifier introduces. */
    locals: string[];
}
/** The import graph of one program. */
export interface ImportGraph {
    nodes: Map<string, FileNode>;
    /** Each file, with the files whose imports resolve into it. */
    importers: Map<string, Set<string>>;
}
/** What an edit changed in one file: its text before and after. */
export interface FileChange {
    file: string;
    before: string;
    after: string;
}
/** `a/b.d.ts`, `a/b.js` and `a/b/index.ts` all look up as `a/b`. */
export declare function stemOf(file: string): string;
/** The top-level names one statement declares. */
export declare function declaredNames(statement: ts.Statement): string[];
/** A file's shape from its text. */
export declare function shapeOf(file: string, text: string): Shape;
/** The names of `shape` that read any of `seeds`, the seeds included. */
export declare function closeOver(shape: Shape, seeds: Iterable<string>): Set<string>;
/**
 * The import graph of `program`'s own (not installed) files, keyed by path
 * relative to `root`.
 */
export declare function importGraphOf(program: ts.Program, root: string): ImportGraph;
/**
 * One graph from the graphs of several programs. A file two programs hold
 * keeps every binding either read, and is a script when either reads it as
 * one: reaching more is the safe side.
 */
export declare function mergeGraphs(graphs: ImportGraph[]): ImportGraph;
/** What an edit reaches: the files, whether an export was added or removed, and the files holding an augmentation. */
export type Reached = {
    files: Set<string>;
    exportsMoved: boolean;
    augmented: Set<string>;
} | 'all';
/**
 * The files `changes` can reach in the program `graph` was read from, or
 * `'all'` when a change reaches every file. Read the graph from the program
 * as it was before the change. A file that is added or deleted has `''` on its
 * missing side.
 */
export declare function affectedFiles(graph: ImportGraph, changes: FileChange[]): Reached;
export declare function serialiseGraph(graph: ImportGraph): SerialGraph;
/** The graph `serialiseGraph` wrote. Throws on an index outside the string table. */
export declare function graphFromSerial(serial: SerialGraph): ImportGraph;
/** What `reach` answers. */
export interface ReachResult {
    reached: string[] | 'all';
    augmented: string[];
    exports_moved: boolean;
}
/** `affectedFiles` over a serialised graph, with the answer sorted. */
export declare function reach(serial: SerialGraph, changes: FileChange[]): ReachResult;
export {};
