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
import { SyntaxKind, ts, } from 'ts-morph';
import { lineIndex } from './line-index.js';
/** How far, in lines, a function may start from the line that names it. */
const LINE_TOLERANCE = 2;
const FUNCTION_KINDS = new Set([
    SyntaxKind.FunctionDeclaration,
    SyntaxKind.ArrowFunction,
    SyntaxKind.FunctionExpression,
    SyntaxKind.MethodDeclaration,
]);
/** The kinds ts-morph's `Node.isStatement` accepts. */
const STATEMENT_KINDS = new Set([
    SyntaxKind.Block,
    SyntaxKind.BreakStatement,
    SyntaxKind.ClassDeclaration,
    SyntaxKind.ContinueStatement,
    SyntaxKind.DebuggerStatement,
    SyntaxKind.DoStatement,
    SyntaxKind.EmptyStatement,
    SyntaxKind.EnumDeclaration,
    SyntaxKind.ExportAssignment,
    SyntaxKind.ExportDeclaration,
    SyntaxKind.ExpressionStatement,
    SyntaxKind.ForInStatement,
    SyntaxKind.ForOfStatement,
    SyntaxKind.ForStatement,
    SyntaxKind.FunctionDeclaration,
    SyntaxKind.IfStatement,
    SyntaxKind.ImportDeclaration,
    SyntaxKind.ImportEqualsDeclaration,
    SyntaxKind.InterfaceDeclaration,
    SyntaxKind.LabeledStatement,
    SyntaxKind.ModuleBlock,
    SyntaxKind.ModuleDeclaration,
    SyntaxKind.NotEmittedStatement,
    SyntaxKind.ReturnStatement,
    SyntaxKind.SwitchStatement,
    SyntaxKind.ThrowStatement,
    SyntaxKind.TryStatement,
    SyntaxKind.TypeAliasDeclaration,
    SyntaxKind.VariableStatement,
    SyntaxKind.WhileStatement,
    SyntaxKind.WithStatement,
]);
const indexes = new WeakMap();
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
export function functionAtLine(sourceFile, line) {
    if (!Number.isFinite(line))
        return undefined;
    const index = indexOf(sourceFile.compilerNode);
    /** Statements opening inside the forward window. */
    const windowStatements = [];
    for (let at = Math.ceil(line); at <= line + LINE_TOLERANCE; at++) {
        const opening = index.statementsByLine.get(at);
        if (opening)
            windowStatements.push(...opening);
    }
    const separatedFromAnchor = (fn) => windowStatements.some((statement) => statement.line < fn.line && !(statement.start <= fn.start && statement.end >= fn.end));
    let best;
    for (let at = Math.ceil(line - LINE_TOLERANCE); at <= line + LINE_TOLERANCE; at++) {
        for (const fn of index.functionsByLine.get(at) ?? []) {
            if (fn.line > line && separatedFromAnchor(fn))
                continue;
            if (best === undefined || isBetter(fn, best, line))
                best = fn;
        }
    }
    return best ? wrapped(sourceFile, best) : undefined;
}
/** Closer to the line, then smaller, then earlier in the file. */
function isBetter(fn, best, line) {
    const delta = Math.abs(fn.line - line);
    const bestDelta = Math.abs(best.line - line);
    if (delta !== bestDelta)
        return delta < bestDelta;
    const size = fn.end - fn.start;
    const bestSize = best.end - best.start;
    if (size !== bestSize)
        return size < bestSize;
    return fn.order < best.order;
}
function indexOf(file) {
    let index = indexes.get(file);
    if (!index) {
        index = buildIndex(file);
        indexes.set(file, index);
    }
    return index;
}
function buildIndex(file) {
    // Lines as ts-morph's `getStartLineNumber` counts them, which is what the
    // scanner's line numbers were matched against.
    const lineAt = lineIndex(file.text);
    const functionsByLine = new Map();
    const statementsByLine = new Map();
    let order = 0;
    const visit = (node) => {
        const isFunction = FUNCTION_KINDS.has(node.kind);
        const isStatement = STATEMENT_KINDS.has(node.kind);
        if (isFunction || isStatement) {
            const start = node.getStart(file);
            const extent = { start, end: node.end, line: lineAt(start) };
            if (isFunction) {
                pushAt(functionsByLine, extent.line, { ...extent, node, order: order++ });
            }
            if (isStatement) {
                pushAt(statementsByLine, extent.line, extent);
            }
        }
        ts.forEachChild(node, visit);
    };
    ts.forEachChild(file, visit);
    return { functionsByLine, statementsByLine };
}
function pushAt(byLine, line, entry) {
    const entries = byLine.get(line);
    if (entries)
        entries.push(entry);
    else
        byLine.set(line, [entry]);
}
/**
 * The ts-morph node for an indexed function. Only this one is wrapped: the
 * deepest node at the function's first character is the function or sits
 * inside it, so its ancestors lead there, and the rest of the file's nodes are
 * never given wrappers they would keep for the life of the project.
 */
function wrapped(sourceFile, fn) {
    let node = sourceFile.getDescendantAtPos(fn.start);
    while (node && node.compilerNode !== fn.node) {
        node = node.getParent();
    }
    // An invariant, not a case: no shape is known where the climb misses. If
    // one exists, the answer is still the function the index chose.
    node ??= sourceFile.getFirstDescendant((descendant) => descendant.compilerNode === fn.node);
    return node;
}
