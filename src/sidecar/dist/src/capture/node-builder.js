/**
 * SymbolTracker-backed node-builder printing for anonymous inferred types
 * (design doc, Capture step 5). This is WP1's risk concentration, built
 * failure-visibility-first: any tracked symbol that is not plainly
 * accessible from the destination file demotes the alias to the
 * structural_fallback tier with a recorded reason -- the bundle never ships
 * a silently wrong .d.ts.
 *
 * The four banked corrections from the 2026-07-02 derisk sweep, applied
 * verbatim:
 *  1. enclosingDeclaration anchors in the DESTINATION surface file (the
 *     entry's placeholder alias for this anchor), never undefined.
 *  2. Demotion triggers on accessibility !== Accessible -- that includes
 *     CannotBeNamed (2), not just NotAccessible (1).
 *  3. Detection drives off the trackSymbol callback; the
 *     reportInaccessibleUniqueSymbolError / reportInaccessibleThisError
 *     callbacks never fire for these shapes and are implemented only as
 *     belt-and-braces recorders.
 *  4. The tracker is passed as the 5th argument of the (internal)
 *     typeToTypeNode signature: (type, enclosingDeclaration, flags,
 *     internalFlags, tracker).
 */
import ts from 'typescript';
/**
 * SymbolAccessibility is internal to the compiler (stable since TS 1.x):
 * Accessible = 0, NotAccessible = 1, CannotBeNamed = 2. Mirrored here
 * because the public .d.ts does not export it; correction 2 depends on the
 * distinction between the two failure members.
 */
const ACCESSIBLE = 0;
/**
 * The node builder can only print out-of-scope symbols as `import("...")`
 * type references when the tracker carries a moduleResolverHost -- without
 * one it returns no node at all for external-package symbols
 * (probe-verified on TS 5.8). Hand-rolling the host is a losing game (it
 * needs a dozen emit-host internals); TS's own factory builds it from the
 * program, which is exactly the "route through the emitter's machinery, not
 * a bare typeToTypeNode" guidance from the derisk sweep.
 */
function moduleResolverHostFor(program) {
    const factory = ts.createModuleSpecifierResolutionHost;
    if (!factory)
        return undefined;
    return factory(program, {
        fileExists: ts.sys.fileExists,
        readFile: ts.sys.readFile,
        directoryExists: ts.sys.directoryExists,
        getCurrentDirectory: () => program.getCurrentDirectory(),
        useCaseSensitiveFileNames: () => ts.sys.useCaseSensitiveFileNames,
    });
}
/**
 * Print `type` as a type node anchored at `destination` (a declaration inside
 * the surface entry file). Returns untrusted-failure instead of text whenever
 * any referenced symbol is not plainly accessible from the destination.
 */
export function printTypeForDestination(program, type, destination) {
    const checker = program.getTypeChecker();
    const inaccessible = [];
    const seen = new Set();
    const record = (symbol, meaning) => {
        if (seen.has(symbol))
            return;
        seen.add(symbol);
        const accessibility = checker.isSymbolAccessible(symbol, destination, // correction 1: destination file, never undefined
        meaning ?? ts.SymbolFlags.Type, 
        /* shouldComputeAliasesToMakeVisible */ false).accessibility;
        // Correction 2: anything other than Accessible demotes -- CannotBeNamed
        // (2) is a distinct enum member that a `=== NotAccessible` check misses.
        if (accessibility !== ACCESSIBLE) {
            inaccessible.push(symbol.getName());
        }
    };
    // Correction 3: trackSymbol is the callback that actually fires. The
    // report* callbacks are dead for the verified failure shapes (unexported
    // local interfaces, unique-symbol keys, local recursive aliases) but are
    // kept as recorders in case other shapes reach them.
    const tracker = {
        moduleResolverHost: moduleResolverHostFor(program),
        trackSymbol: (symbol, enclosing, meaning) => {
            void enclosing;
            record(symbol, meaning);
            // Returning false tells the builder no error was reported here; the
            // demotion decision is ours, made after the walk completes.
            return false;
        },
        reportInaccessibleThisError: () => {
            inaccessible.push('this');
        },
        reportInaccessibleUniqueSymbolError: () => {
            inaccessible.push('(unique symbol)');
        },
        reportPrivateInBaseOfClassExpression: (propertyName) => {
            inaccessible.push(propertyName);
        },
    };
    let node;
    try {
        node = checker.typeToTypeNode(type, destination, ts.NodeBuilderFlags.NoTruncation |
            ts.NodeBuilderFlags.UseStructuralFallback |
            ts.NodeBuilderFlags.InTypeAlias, 
        /* internalFlags */ undefined, tracker // correction 4: 5th argument
        );
    }
    catch (err) {
        return {
            inaccessible,
            failure: `node builder threw: ${err instanceof Error ? err.message : String(err)}`,
        };
    }
    if (!node) {
        return { inaccessible, failure: 'node builder returned no type node' };
    }
    if (inaccessible.length > 0) {
        return {
            inaccessible,
            failure: `symbols not accessible from the surface entry: ${[...new Set(inaccessible)].join(', ')}`,
        };
    }
    // carrick#1377: a reference nothing declares makes the WHOLE answer
    // unpublishable, so one member typed by a package the checkout does not
    // have used to discard every member around it. Replace what it names with
    // `unknown` in place and print that; the rest of the shape survives, and
    // the positions are reported so the finding at each can be labelled as a
    // module that did not resolve rather than a top type the author declared.
    const substituted = substituteUndeclaredNames(node, program, destination);
    const printer = ts.createPrinter({ removeComments: true });
    const text = printer.printNode(ts.EmitHint.Unspecified, substituted.node, destination.getSourceFile());
    // Asked of the REWRITTEN node: the field says what the printed answer
    // names, so anything the substitution could not reach still fills it and
    // still refuses publication.
    const undeclaredNames = undeclaredNamesIn(substituted.node, program, destination);
    const substitutedPaths = substituted.substitutions.map((entry) => entry.path);
    return {
        text,
        inaccessible,
        ...(undeclaredNames.length > 0 ? { undeclaredNames } : {}),
        ...(substitutedPaths.length > 0 ? { substitutedPaths } : {}),
    };
}
/**
 * Rewrite every reference in `node` that nothing in the producer's program
 * declares to the `unknown` keyword, and report the member position each sat
 * at (carrick#1377).
 *
 * The positions use the same notation as the capture's deep walk — `sub`,
 * `items<0>.meta`, `[index]`, `()` for a callable return — so a path found
 * here names the same member a self-check finding at that path names.
 */
export function substituteUndeclaredNames(node, program, destination) {
    const undeclared = new Set(undeclaredNamesIn(node, program, destination));
    if (undeclared.size === 0) {
        return { node, substitutions: [] };
    }
    const substitutions = [];
    const leftmost = (name) => ts.isIdentifier(name) ? name : leftmost(name.left);
    // The path is carried down the visit rather than reconstructed, because a
    // rewritten node has no parent to walk back up from.
    const rewrite = (current, path) => {
        if ((ts.isTypeReferenceNode(current) && undeclared.has(leftmost(current.typeName).text)) ||
            (ts.isTypeQueryNode(current) && undeclared.has(leftmost(current.exprName).text))) {
            substitutions.push({
                name: ts.isTypeReferenceNode(current)
                    ? leftmost(current.typeName).text
                    : leftmost(current.exprName).text,
                path: path === '' ? '<root>' : path,
            });
            return ts.factory.createKeywordTypeNode(ts.SyntaxKind.UnknownKeyword);
        }
        return ts.visitEachChild(current, (child) => rewrite(child, childPath(current, child, path)), 
        /* context */ undefined);
    };
    const rewritten = rewrite(node, '');
    return { node: rewritten, substitutions };
}
/** The deep walk's path for `child` inside `parent`, extending `path`. */
export function childPath(parent, child, path) {
    if (ts.isPropertySignature(parent) && parent.type === child) {
        const name = ts.isIdentifier(parent.name) || ts.isStringLiteral(parent.name)
            ? parent.name.text
            : parent.name.getText?.() ?? '';
        return path === '' ? name : `${path}.${name}`;
    }
    if (ts.isArrayTypeNode(parent) && parent.elementType === child) {
        return `${path}<0>`;
    }
    if (ts.isIndexSignatureDeclaration(parent) && parent.type === child) {
        return `${path}[index]`;
    }
    if ((ts.isFunctionTypeNode(parent) ||
        ts.isMethodSignature(parent) ||
        ts.isCallSignatureDeclaration(parent)) &&
        parent.type === child) {
        return `${path}()`;
    }
    if (ts.isTypeReferenceNode(parent) && parent.typeArguments) {
        const index = parent.typeArguments.indexOf(child);
        if (index >= 0)
            return `${path}<${index}>`;
    }
    if (ts.isTupleTypeNode(parent)) {
        const index = parent.elements.indexOf(child);
        if (index >= 0)
            return `${path}<${index}>`;
    }
    // A union or intersection member sits at its parent's position, as the deep
    // walk records it; everything else (a type literal's members, a parenthesis)
    // keeps the path it was reached with.
    return path;
}
/**
 * Bare names in a printed type node that nothing in the producer's program
 * declares (carrick#1165).
 *
 * The builder names an out-of-scope declaration through `import("...")`, and
 * the tracker demotes a symbol it cannot reach, so a bare reference is meant
 * to be in scope where the alias is declared. The one way it is not: the
 * builder REUSES a source annotation as written (`parcel: Row`) when the
 * annotation's import did not resolve. There is then no symbol to track, and
 * the surface names an identifier nothing declares. Literal anchor text, which
 * the v1 walk printed, can name a type the same way.
 *
 * A name is listed only when it neither resolves at the destination (every
 * lib, runtime and `@types` global does) nor has a declaration anywhere in the
 * program. The second test matters on a healthy checkout: the v1 walk prints
 * an enum member (`Status.Open`) or a recursive reference by name, and that
 * name is declared in the project though not in scope at the surface. An
 * import binding is not a declaration, so a name whose only source is an
 * import that did not resolve is still listed. Type parameters the print
 * itself declares (generic signatures, mapped and `infer` types) are excluded.
 */
export function undeclaredNamesIn(node, program, destination) {
    const checker = program.getTypeChecker();
    const typeParameters = new Set();
    const leftmost = (name) => ts.isIdentifier(name) ? name : leftmost(name.left);
    const references = [];
    const visit = (current) => {
        if (ts.isTypeParameterDeclaration(current)) {
            typeParameters.add(current.name.text);
        }
        else if (ts.isTypeReferenceNode(current)) {
            references.push({ name: leftmost(current.typeName).text, space: 'types' });
        }
        else if (ts.isTypeQueryNode(current)) {
            references.push({ name: leftmost(current.exprName).text, space: 'values' });
        }
        ts.forEachChild(current, visit);
    };
    visit(node);
    const undeclared = new Set();
    for (const { name, space } of references) {
        if (typeParameters.has(name) || undeclared.has(name))
            continue;
        const meaning = (space === 'types' ? ts.SymbolFlags.Type : ts.SymbolFlags.Value) |
            ts.SymbolFlags.Namespace |
            ts.SymbolFlags.Alias;
        if (checker.resolveName(name, destination, meaning, false))
            continue;
        if (declaredNamesOf(program)[space].has(name))
            continue;
        undeclared.add(name);
    }
    return [...undeclared].sort();
}
const declaredNamesCache = new WeakMap();
/**
 * Every name a declaration in the program introduces, split by the space it
 * can be referenced from: at any depth of a source file and at any namespace
 * depth of a declaration file. Import bindings are left out on purpose (see
 * `undeclaredNamesIn`). Built once per program.
 */
function declaredNamesOf(program) {
    const cached = declaredNamesCache.get(program);
    if (cached)
        return cached;
    const names = { types: new Set(), values: new Set() };
    const record = (name, types, values) => {
        if (!name || !ts.isIdentifier(name))
            return;
        if (types)
            names.types.add(name.text);
        if (values)
            names.values.add(name.text);
    };
    const visit = (current, deep) => {
        if (ts.isInterfaceDeclaration(current) || ts.isTypeAliasDeclaration(current)) {
            record(current.name, true, false);
        }
        else if (ts.isClassDeclaration(current) ||
            ts.isEnumDeclaration(current) ||
            ts.isModuleDeclaration(current)) {
            // A namespace can qualify a type reference (`Ns.Row`) and a query.
            record(current.name, true, true);
        }
        else if (ts.isFunctionDeclaration(current) || ts.isVariableDeclaration(current)) {
            record(current.name, false, true);
        }
        // A declaration file has no bodies to hide a declaration in, so its
        // statement lists (and namespace blocks) are the whole story; a lib or
        // package file is walked no deeper than that.
        if (deep ||
            ts.isSourceFile(current) ||
            ts.isModuleDeclaration(current) ||
            ts.isModuleBlock(current) ||
            ts.isVariableStatement(current) ||
            ts.isVariableDeclarationList(current)) {
            ts.forEachChild(current, (child) => visit(child, deep));
        }
    };
    for (const sourceFile of program.getSourceFiles()) {
        visit(sourceFile, !sourceFile.isDeclarationFile);
    }
    declaredNamesCache.set(program, names);
    return names;
}
