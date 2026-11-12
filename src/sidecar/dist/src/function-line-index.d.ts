/**
 * The function a line names, from an index built once per source file
 * (carrick#1915).
 *
 * A signature request carries only the line its function starts on, and a
 * signature pass sends one request per unannotated slot: thousands of
 * lookups, several per function and many per file. Answering each by walking
 * the file's every node cost the file's size per slot, and was 81% of the
 * pass on a 2,345-file program. Here the file is walked once, over the
 * compiler's own nodes, and each lookup reads the few entries near its line.
 *
 * The index belongs to the compiler's source file node, not to the file's
 * name: replacing a file's text gives it a new node, so a file rewritten for
 * one reading (the unwidened reading, the retype check) and then restored is
 * indexed again each time, and never answered from positions it no longer has.
 */
import { type ArrowFunction, type FunctionDeclaration, type FunctionExpression, type MethodDeclaration, type SourceFile } from 'ts-morph';
/** The declarations a line can name. */
export type LineFunction = FunctionDeclaration | ArrowFunction | FunctionExpression | MethodDeclaration;
/**
 * The function whose declaration starts at, or within `LINE_TOLERANCE` lines
 * of, the given line: the closest, then the smallest, then the first in the
 * file. A function starting after the line is passed over when a statement
 * that opens on the line or after it, on a line before the function's, does
 * not contain the function. Undefined when no function is left.
 *
 * These are `TypeInferrer.findFunctionByLine`'s rules, which says why each is
 * there; this is where they are computed.
 */
export declare function functionAtLine(sourceFile: SourceFile, line: number): LineFunction | undefined;
