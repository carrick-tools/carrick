/**
 * No stub names a module by a machine path (carrick#2943).
 *
 * The specifier rewrite maps an absolute specifier onto a file of the stub's
 * tree or onto an installed package. What it cannot map stays as printed: a
 * type that a printer could name only by where its module sits on this
 * machine, such as a workspace sibling's source reached through a
 * `node_modules` link, which the declaration emit does not carry. That path
 * resolves on the machine that ran the capture, to a source file outside the
 * stub, and nowhere else: the upload rewrites it to `<checkout>/...`, so the
 * stored stub and a fresh one disagree about the same alias.
 *
 * Each such reference becomes `unknown` at its own position. In the surface
 * the position is reported per alias, so the self-check labels the finding
 * there as a reference that did not resolve rather than as declared; the rest
 * of the alias's type survives. Any other tree file gets the dangling-import
 * repair (carrick#1397), which writes `unknown` where the import's names were
 * used. The stub then reads the same on every machine.
 *
 * Seam: node builtins + `typescript`, like the rest of this directory.
 */
import type { WriteGuard } from './guarded-fs.js';
/** A specifier that names a module by where it sits on one machine. */
export declare function isMachinePath(specifier: string): boolean;
/** What the surface substitution replaced in one alias. */
export interface MachinePathPositions {
    /** Member paths now `unknown`, in the deep walk's notation (`''` = root). */
    paths: string[];
    /** The modules they named, relative to `labelRoot` (never absolute). */
    modules: string[];
}
/**
 * Replace every machine-path import in the stub's tree with `unknown`.
 * `files` are absolute tree paths; `surfaceAbsPath` is one of them. Returns
 * the positions replaced in the surface, by alias.
 */
export declare function substituteMachinePathImports(args: {
    files: readonly string[];
    surfaceAbsPath: string;
    /** A guard over the stub's types tree. */
    tree: WriteGuard;
    /** The root a module is named from in a label (the scanned repo's). */
    labelRoot: string;
}): Map<string, MachinePathPositions>;
