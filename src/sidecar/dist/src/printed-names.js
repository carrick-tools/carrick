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
import { ts } from 'ts-morph';
import { typeToTypeNodeAsDeclared } from './print-type.js';
/** The prints of the inference running now, when one is recording. */
let recording;
/**
 * Note a type the compiler printed as `text`, so the inference running now can
 * tell what the names in that print meant. Does nothing outside a recording.
 */
export function notePrintedType(type, enclosing, text) {
    recording?.push({ type: type.compilerType, enclosing: enclosing?.compilerNode, text });
}
/**
 * Same flags the printers pass `typeToString` (`NoTruncation`, `InTypeAlias`),
 * so the node read back is the one that was printed.
 */
const NODE_FLAGS = ts.NodeBuilderFlags.NoTruncation | ts.NodeBuilderFlags.InTypeAlias | ts.NodeBuilderFlags.IgnoreErrors;
const REFERENCE_MEANING = ts.SymbolFlags.Type | ts.SymbolFlags.Namespace | ts.SymbolFlags.Alias;
/** The type prints one inference made. */
export class PrintedTypes {
    prints = [];
    /** Run `read`, noting every type it prints. */
    during(read) {
        const outer = recording;
        recording = this.prints;
        try {
            return read();
        }
        finally {
            recording = outer;
        }
    }
    /**
     * The declarations behind each name `texts` print that `source` does not
     * resolve, sorted, one entry per distinct declaration. `checker` must be
     * the one the prints were made with: call this before the program changes.
     */
    namesIn(texts, source, checker) {
        const wanted = new Set();
        for (const text of texts) {
            for (const name of referencedNames(text)) {
                if (!checker.resolveName(name, source, REFERENCE_MEANING, false))
                    wanted.add(name);
            }
        }
        if (wanted.size === 0)
            return [];
        // Only a print whose text writes one of the names can say what it meant.
        const alternatives = [...wanted].map((name) => name.replace(/\$/g, '\\$')).join('|');
        const writes = new RegExp(`(?<![\\w$.])(?:${alternatives})(?![\\w$])`);
        const found = new Map();
        const identities = new Map();
        const visited = new Map();
        for (const { type, enclosing, text } of this.prints) {
            if (!writes.test(text))
                continue;
            const at = visited.get(type) ?? new Set();
            if (at.has(enclosing))
                continue;
            visited.set(type, at.add(enclosing));
            let node;
            try {
                node = typeToTypeNodeAsDeclared(checker, type, enclosing, NODE_FLAGS);
            }
            catch {
                continue;
            }
            const visit = (current) => {
                if (ts.isTypeReferenceNode(current)) {
                    const id = leftmost(current.typeName);
                    const name = ts.idText(id);
                    const symbol = wanted.has(name) ? symbolOf(id, checker) : undefined;
                    if (symbol) {
                        if (!identities.has(symbol))
                            identities.set(symbol, identityOf(symbol, checker));
                        const identity = identities.get(symbol);
                        if (identity) {
                            found.set(`${name}\0${identity.file}\0${identity.export_path.join('.')}`, { name, ...identity });
                        }
                    }
                }
                ts.forEachChild(current, visit);
            };
            if (node)
                visit(node);
        }
        return [...found.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([, entry]) => entry);
    }
}
function leftmost(name) {
    return ts.isIdentifier(name) ? name : leftmost(name.left);
}
/**
 * The leftmost names of the type references `text` makes, less the type
 * parameters the text declares itself (`[K in ...]`, `infer U`).
 */
function referencedNames(text) {
    const parsed = ts.createSourceFile('printed.ts', `type __Printed = ${text};`, ts.ScriptTarget.Latest, true);
    const names = new Set();
    const declared = new Set();
    const visit = (node) => {
        if (ts.isTypeParameterDeclaration(node))
            declared.add(node.name.text);
        if (ts.isTypeReferenceNode(node))
            names.add(ts.idText(leftmost(node.typeName)));
        ts.forEachChild(node, visit);
    };
    visit(parsed);
    for (const name of declared)
        names.delete(name);
    names.delete('__Printed');
    return names;
}
/**
 * The symbol a printed name was written for. The node builder sets it on
 * every identifier it creates (`symbol` is not in the public typings); a name
 * it copied from source carries none, and is read at its original node.
 */
function symbolOf(id, checker) {
    const written = id.symbol;
    if (written)
        return written;
    const original = ts.getOriginalNode(id);
    return original !== id && original.parent ? checker.getSymbolAtLocation(original) : undefined;
}
/**
 * The module that declares `symbol`'s type and the export path to it there,
 * or undefined for a type no module export reaches: a global, a type
 * parameter, a declaration a function body holds, a module itself.
 */
function identityOf(symbol, checker) {
    const resolve = (s) => s.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(s) : s;
    const target = resolve(symbol);
    if (!(target.flags & (ts.SymbolFlags.Type | ts.SymbolFlags.Namespace)) ||
        target.flags & ts.SymbolFlags.TypeParameter) {
        return undefined;
    }
    const declaration = target.declarations?.[0];
    if (!declaration || ts.isSourceFile(declaration))
        return undefined;
    const file = declaration.getSourceFile();
    const moduleSymbol = checker.getSymbolAtLocation(file);
    if (!moduleSymbol)
        return undefined;
    const direct = checker.getExportsOfModule(moduleSymbol).filter((e) => resolve(e) === target);
    if (direct.length > 0) {
        const chosen = direct.find((e) => e.getName() === target.getName()) ?? direct[0];
        return { file: file.fileName, export_path: [chosen.getName()] };
    }
    // A namespace member: `"<module>".Billing.Kind`, checked step by step.
    const qualified = checker.getFullyQualifiedName(target);
    const close = qualified.startsWith('"') ? qualified.indexOf('"', 1) : -1;
    if (close < 0)
        return undefined;
    const exportPath = qualified.slice(close + 2).split('.');
    let current = moduleSymbol;
    for (const part of exportPath) {
        const next = current
            ? checker.getExportsOfModule(current).find((e) => e.getName() === part)
            : undefined;
        current = next && resolve(next);
    }
    return current === target ? { file: file.fileName, export_path: exportPath } : undefined;
}
