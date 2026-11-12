/**
 * The implementations of a bodyless declaration, asked where discovery stops
 * at one (carrick#2432 slice B): a call through an interface's method, a
 * function-typed property or an abstract member, whose receiver the scanner
 * cannot follow to a value.
 *
 * Each question is the span of a `call` (or `member`) position row, in the
 * rows' numbering (UTF-16 code units from the start of the file, as the
 * scanner's `sidecar_position` gives). The call's symbol and declaration are
 * read exactly as the row read them ({@link PositionWalker.symbolOf}), and
 * go-to-implementation is asked once per declaration, at its name, over the
 * language service's program of the project that owns the file. That is the
 * analysis program: nothing here releases it.
 *
 * Each answer:
 * - holds none of the declaration's own signatures: asked at an overloaded
 *   interface or abstract method, go-to-implementation names only the bodies
 *   that implement it (TypeScript 6.0, pinned in implementations.test.ts).
 *   The walk's filter for them (`implementationRows`, removed in #2561) is
 *   not carried over: no bodyless declaration reaches it;
 * - names each implementation as a row names a declaration
 *   ({@link PositionWalker.located}), at the implementation's declaration's
 *   start, so it joins to a definition as a `call` row at it would;
 * - is sorted by file, then position;
 * - holds at most {@link IMPLEMENTATIONS_CAP}; past it, none, and `too_many`.
 *
 * The position rows carry no implementations (version 4, carrick#2561): this
 * action is the only place they are asked.
 */
import * as path from 'node:path';
import { ts } from 'ts-morph';
import { attributedDeclaration } from './declaration-attribution.js';
import { PathNames, PositionWalker, compareNames, kindOf } from './position-rows.js';
/** The most implementations one answer lists. */
export const IMPLEMENTATIONS_CAP = 16;
/** One answer per question, in request order. */
export function findImplementations(options) {
    const { loader, scanRoot, queries } = options;
    const paths = new PathNames(scanRoot);
    const answers = new Array(queries.length);
    const groups = new Map();
    queries.forEach((query, index) => {
        const key = loader.projectKeyFor(path.resolve(query.file));
        groups.set(key, [...(groups.get(key) ?? []), index]);
    });
    for (const [key, indexes] of groups) {
        const service = loader.getProjectFor(key).getLanguageService().compilerObject;
        const program = service.getProgram();
        if (!program)
            throw new Error('the language service built no program');
        const finder = new ImplementationFinder(program, service, new PositionWalker(program, paths, loader));
        for (const index of indexes) {
            answers[index] = finder.answer(queries[index]);
            options.onAnswer?.();
        }
    }
    return answers;
}
/** An answer with nothing found, and why. */
function none(reason, declaration = null) {
    return { declaration, implementations: [], too_many: false, reason };
}
class ImplementationFinder {
    program;
    service;
    walker;
    checker;
    /** Answers by declaration: a declaration is searched once, however many calls reach it. */
    searched = new Map();
    constructor(program, service, walker) {
        this.program = program;
        this.service = service;
        this.walker = walker;
        this.checker = program.getTypeChecker();
    }
    answer(query) {
        const sourceFile = this.program.getSourceFile(path.resolve(query.file));
        if (!sourceFile)
            return none('not_in_program');
        const found = positionAt(sourceFile, query.start, query.end);
        if (!found)
            return none('no_call');
        let declaration;
        let named = null;
        try {
            const symbol = this.walker.symbolOf(found.kind, found.node);
            if (!symbol)
                return none('no_symbol');
            declaration = attributedDeclaration(() => this.program, symbol.declarations ?? []);
            if (!declaration)
                return none('no_declaration');
            named = this.named(declaration, ts.symbolName(symbol));
            if (!isBodyless(declaration, this.checker))
                return none('has_body', named);
        }
        catch {
            return none('checker_threw', named);
        }
        let searched = this.searched.get(declaration);
        if (!searched) {
            try {
                searched = this.implementationsOf(declaration);
            }
            catch {
                return none('checker_threw', named);
            }
            this.searched.set(declaration, searched);
        }
        return { declaration: named, ...searched };
    }
    /** A declaration as a row names it: its symbol, file and line. */
    named(declaration, symbol) {
        const file = declaration.getSourceFile();
        const { declFile, declLine, declPackage } = this.walker.located(file, declaration.getStart(file));
        return { symbol, decl_file: declFile, decl_line: declLine, decl_package: declPackage };
    }
    /**
     * What go-to-implementation names for `declaration`, asked at its name,
     * in file and position order.
     */
    implementationsOf(declaration) {
        const name = ts.getNameOfDeclaration(declaration) ?? declaration;
        const declaringFile = name.getSourceFile();
        const found = new Map();
        for (const location of this.service.getImplementationAtPosition(declaringFile.fileName, name.getStart(declaringFile)) ?? []) {
            const file = this.program.getSourceFile(location.fileName);
            if (!file)
                continue;
            const start = location.textSpan.start;
            found.set(`${location.fileName}\u0000${start}`, { file, start });
        }
        if (found.size > IMPLEMENTATIONS_CAP)
            return { implementations: [], too_many: true };
        const implementations = [...found.values()].map(({ file, start }) => {
            const at = nodeStartingAt(file, start);
            const symbol = at ? this.checker.getSymbolAtLocation(at) : undefined;
            // The declaration the answer points into, so its line is the one a
            // `call` row at the implementation names: the declaration's start,
            // decorators and modifiers included, not its name's.
            const holder = symbol?.declarations?.find((d) => d.getSourceFile() === file && d.getStart(file) <= start && start < d.end) ??
                (at ? enclosingDeclaration(at) : undefined);
            const placed = holder ? holder.getStart(file) : start;
            const { declFile, declLine, declPackage } = this.walker.located(file, placed);
            return {
                file: declFile,
                start: placed,
                named: { symbol: symbol ? ts.symbolName(symbol) : null, decl_file: declFile, decl_line: declLine, decl_package: declPackage },
            };
        });
        implementations.sort((a, b) => compareNames(a.file, b.file) || a.start - b.start);
        return { implementations: implementations.map(({ named }) => named), too_many: false };
    }
}
/**
 * The `call` or `member` position whose span is exactly `start..end`: the
 * node a row of that kind was written at. A call is preferred where both
 * kinds share the span.
 */
function positionAt(file, start, end) {
    let member;
    let call;
    const visit = (node) => {
        const nodeStart = node.getStart(file);
        if (end <= nodeStart || node.getEnd() < start)
            return;
        if (nodeStart === start && node.getEnd() === end) {
            const kind = kindOf(node);
            if (kind === 'call')
                call ??= node;
            if (kind === 'member')
                member ??= node;
        }
        ts.forEachChild(node, visit);
    };
    ts.forEachChild(file, visit);
    if (call)
        return { kind: 'call', node: call };
    if (member)
        return { kind: 'member', node: member };
    return undefined;
}
/**
 * Whether a declaration has no body for a call to run: an interface's or type
 * literal's method, a function-typed property signature, an abstract method or
 * property, or a function-typed class property or parameter with no
 * initializer. Its implementations are what go-to-implementation names.
 */
function isBodyless(declaration, checker) {
    const functionTyped = () => checker.getTypeAtLocation(declaration).getNonNullableType().getCallSignatures().length > 0;
    if (ts.isMethodSignature(declaration))
        return true;
    if (ts.isPropertySignature(declaration))
        return functionTyped();
    if (ts.isMethodDeclaration(declaration) || ts.isPropertyDeclaration(declaration)) {
        if (ts.getCombinedModifierFlags(declaration) & ts.ModifierFlags.Abstract)
            return true;
        return ts.isPropertyDeclaration(declaration) && !declaration.initializer && functionTyped();
    }
    if (ts.isParameter(declaration))
        return !declaration.initializer && functionTyped();
    return false;
}
/** The innermost node of `file` that starts at `position` (an implementation's name). */
function nodeStartingAt(file, position) {
    let found;
    const visit = (node) => {
        if (position < node.getStart(file) || position >= node.getEnd())
            return;
        if (node.getStart(file) === position)
            found = node;
        ts.forEachChild(node, visit);
    };
    ts.forEachChild(file, visit);
    return found;
}
/** The nearest declaration holding `node`: what an answer with no symbol points into. */
function enclosingDeclaration(node) {
    for (let at = node; at && !ts.isSourceFile(at); at = at.parent) {
        if (ts.isFunctionLike(at) || ts.isClassLike(at) || ts.isPropertyAssignment(at) || ts.isVariableDeclaration(at)) {
            return at;
        }
    }
    return undefined;
}
