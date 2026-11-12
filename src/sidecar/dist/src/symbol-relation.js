/**
 * How a type the model named for a producer's response relates to the
 * compiler's type of what the handler sends (carrick#2111).
 *
 * The model names one symbol per row, and by schema it is a bare name. The
 * compiler reads the response expression itself. Where the two are the same
 * type the name is the better thing to publish; where they are not, the name
 * is wrong about the body: an element named for a list, a helper's input type
 * named for its result, a type named for the wrapper sent around it.
 *
 * The compiler decides, by assignability asked both ways. Neither names nor
 * printed text are compared.
 */
import { ts } from 'ts-morph';
/** A list level deeper than this is not a payload shape worth following. */
const MAX_LIST_DEPTH = 10;
const NULLISH = ts.TypeFlags.Null | ts.TypeFlags.Undefined | ts.TypeFlags.Void;
const TOP = ts.TypeFlags.Any | ts.TypeFlags.Unknown;
function partsOf(types) {
    const members = [];
    let nullish = false;
    for (const type of types) {
        for (const part of type.isUnion() ? type.types : [type]) {
            if (part.flags & NULLISH)
                nullish = true;
            else if (!members.includes(part))
                members.push(part);
        }
    }
    return { members, nullish };
}
/**
 * `type` with its list levels peeled, whichever of `T[]`, `readonly T[]` and
 * a tuple each level is. JSON has no `readonly`, so a list's own modifier is set
 * aside; one deeper inside the element is the wire transform's to drop.
 */
function peelLists(checker, type) {
    let element = type;
    let depth = 0;
    while (depth < MAX_LIST_DEPTH && (checker.isArrayType(element) || checker.isTupleType(element))) {
        // A tuple is a list on the wire: its element is every member's type.
        const inner = checker.getIndexTypeOfType(element, ts.IndexKind.Number);
        if (!inner)
            break;
        element = inner;
        depth += 1;
    }
    return { element, depth };
}
/**
 * The relation of `symbol` (the type the model named) to `read` (the types
 * of the expressions the handler sends, one per surviving branch), or
 * `undefined` where there is nothing to compare: either side is `any` or
 * `unknown` at its root, or is only `null`.
 *
 *  - `same`: the two are one type. A list's own `readonly` does not count.
 *  - `list`: the read is a list of the symbol, `depth` levels deep.
 *  - `member`: the symbol is one member of the read's union, and the rest
 *    is more than `null` or `undefined`. Which member is the success body is
 *    decided by status, elsewhere.
 *  - `nullable`: the read is the symbol or `null`/`undefined`.
 *  - `different`: anything else.
 */
export function relateSymbolToRead(checker, symbol, read) {
    if (read.length === 0)
        return undefined;
    if ([symbol, ...read].some((type) => type.flags & TOP))
        return undefined;
    const sent = partsOf(read);
    const named = partsOf([symbol]);
    if (sent.members.length === 0 || named.members.length === 0)
        return undefined;
    const same = (a, b) => checker.isTypeAssignableTo(a, b) && checker.isTypeAssignableTo(b, a);
    /** One type, up to `readonly` on its list levels. */
    const sameBody = (a, b) => {
        if (same(a, b))
            return true;
        const left = peelLists(checker, a);
        const right = peelLists(checker, b);
        return left.depth > 0 && left.depth === right.depth && same(left.element, right.element);
    };
    const namedAreSent = named.members.every((n) => sent.members.some((s) => sameBody(s, n)));
    const sentAreNamed = sent.members.every((s) => named.members.some((n) => sameBody(s, n)));
    if (namedAreSent && sentAreNamed) {
        if (sent.nullish === named.nullish)
            return { relation: 'same' };
        return { relation: sent.nullish ? 'nullable' : 'different' };
    }
    if (namedAreSent && (sent.nullish || !named.nullish)) {
        return { relation: 'member' };
    }
    if (sent.members.length === 1 && !sent.nullish && !named.nullish) {
        const list = peelLists(checker, sent.members[0]);
        const element = named.members.length === 1 ? peelLists(checker, symbol) : { element: symbol, depth: 0 };
        if (list.depth > element.depth && same(list.element, element.element)) {
            return { relation: 'list', depth: list.depth - element.depth };
        }
    }
    return { relation: 'different' };
}
/**
 * Whether a printed type holds `any` or `unknown` anywhere. Such a read has
 * a part the compiler could not see, and assignability against it says
 * nothing about the part it could.
 */
export function printedTypeHoldsTopType(text) {
    const withoutLiterals = text.replace(/'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"|`(?:[^`\\]|\\.)*`/g, "''");
    return /\b(?:any|unknown)\b/.test(withoutLiterals);
}
