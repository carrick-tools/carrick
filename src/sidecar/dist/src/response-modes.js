/**
 * Which incoming-message field picks each member of a response union (carrick#2054).
 *
 * A handler that sends `{ x }` when `?mode=a` and `{ y }` otherwise publishes
 * `{ x } | { y }`. A call that states `mode=a` receives only `{ x }`, and a
 * call that states nothing may receive either. The union alone cannot say
 * which, so this reads it from the handler: for each success body the union
 * joined, the tests on its path that compare a value read from the incoming message
 * with a string literal.
 *
 * The reading, in order:
 *
 *  - A test counts only when it separates two of the joined bodies: one on
 *    each side of it, or, for an earlier `if` that cannot complete, one in
 *    its branch and one after it. A validation guard whose branch only sends
 *    an error separates nothing the join kept, and is ignored.
 *  - A counted test is a message read when a value it tests is read from a
 *    parameter of a function enclosing the send (`messageRead`). A test that
 *    reads nothing from the incoming message (server state, a local count) is ignored,
 *    so a body under it is under no test of the field and stays in every case.
 *  - `===`, `==`, `!==` and `!=` against a string literal, with `!` flipping
 *    the test, and `switch` labels, are read as values. Any other test of a
 *    message read leaves its field read with its values unread.
 *  - `reads` lists every message read among the counted tests. `cases` is
 *    read only for one placed field whose tests were all read: one case per
 *    literal compared, plus "any other value" (`value: null`) when a body
 *    admits one, each the union of the bodies it admits.
 *  - When every case is the same union, the route has no modes.
 *
 * The published union is not changed by any of this.
 */
import { Node, SyntaxKind, VariableDeclarationKind } from 'ts-morph';
import { cannotComplete, stepsOnPath } from './failure-path.js';
const MAX_CHAIN = 32;
/**
 * The modes of the union joined from `bodies`, or `undefined` when no message
 * read separates them. `join` builds a union text from member texts with the
 * rules the published union is built with. `isBodyRead` says whether a call
 * is the platform's `json()` body read (`platform-body-read.ts`, blueprint
 * row N33).
 */
export function readResponseModes(bodies, join, isBodyRead) {
    if (bodies.length < 2)
        return undefined;
    const boundary = smallestEnclosingFunction(bodies.map((body) => body.node));
    // Each body's side of each test on its path, keyed by the test.
    // A switch some body follows is read as that, whichever body met it first.
    const sides = new Map();
    bodies.forEach((body, index) => {
        for (const step of stepsOnPath(body.node, boundary)) {
            const entry = sides.get(step.test) ?? { step, byBody: new Map() };
            if (step.kind === 'after-switch')
                entry.step = step;
            entry.byBody.set(index, sideKey(step));
            sides.set(step.test, entry);
        }
    });
    const reads = new Map();
    const unreadFields = new Set();
    const literals = new Map();
    const constraints = bodies.map(() => new Map());
    for (const { step, byBody } of sides.values()) {
        if (new Set(byBody.values()).size < 2)
            continue;
        const reading = readStep(step, isBodyRead);
        if (!reading)
            continue;
        if (reading.kind === 'unread') {
            for (const read of reading.reads) {
                reads.set(readKey(read), read);
                unreadFields.add(readKey(read));
            }
            continue;
        }
        const key = readKey(reading.read);
        reads.set(key, reading.read);
        const values = literals.get(key) ?? new Set();
        for (const literal of reading.literals)
            values.add(literal);
        literals.set(key, values);
        for (const [index, side] of byBody) {
            const list = constraints[index].get(key) ?? [];
            list.push(reading.admits(side));
            constraints[index].set(key, list);
        }
    }
    if (reads.size === 0)
        return undefined;
    const sortedReads = [...reads.values()]
        .map(({ location, field }) => ({ location, field }))
        .sort((a, b) => compare(a.location, b.location) || compare(a.field, b.field));
    const readsOnly = { reads: sortedReads };
    if (sortedReads.length !== 1 || sortedReads[0].location === 'unplaced')
        return readsOnly;
    const key = readKey(sortedReads[0]);
    if (unreadFields.has(key))
        return readsOnly;
    const admitted = constraints.map((byField) => intersect(byField.get(key) ?? []));
    const unionFor = (admits) => {
        const texts = bodies.filter((_, index) => admits(admitted[index])).map((body) => body.type_string);
        return texts.length > 0 ? join(texts) : undefined;
    };
    const cases = [];
    for (const value of [...(literals.get(key) ?? [])].sort(compare)) {
        const text = unionFor((body) => admitsValue(body, value));
        // A named value no joined body answers: the reading is not one to narrow on.
        if (text === undefined)
            return readsOnly;
        cases.push({ value, type_string: text });
    }
    const other = unionFor((body) => 'except' in body);
    if (other !== undefined)
        cases.push({ value: null, type_string: other });
    if (new Set(cases.map((c) => c.type_string)).size < 2)
        return undefined;
    return { reads: sortedReads, cases };
}
function compare(a, b) {
    return a < b ? -1 : a > b ? 1 : 0;
}
function readKey(read) {
    return `${read.location}\u0000${read.field}`;
}
/** Which side of its test a step puts a node on. */
function sideKey(step) {
    switch (step.kind) {
        case 'branch':
            return step.whenTrue ? 'true' : 'false';
        case 'clause':
            return `clause:${step.test.getClauses().indexOf(step.clause)}`;
        case 'after-switch':
            return 'after';
    }
}
/** The innermost function that holds every node, `undefined` when none does. */
function smallestEnclosingFunction(nodes) {
    for (const candidate of nodes[0].getAncestors()) {
        if (!Node.isFunctionLikeDeclaration(candidate))
            continue;
        if (nodes.every((node) => node === candidate || candidate.containsRange(node.getPos(), node.getEnd()))) {
            return candidate;
        }
    }
    return undefined;
}
function intersect(list) {
    const every = { except: new Set() };
    let result = every;
    for (const next of list) {
        const current = result;
        if ('only' in next) {
            result = { only: new Set([...next.only].filter((value) => admitsValue(current, value))) };
        }
        else if ('only' in current) {
            result = { only: new Set([...current.only].filter((value) => admitsValue(next, value))) };
        }
        else {
            result = { except: new Set([...current.except, ...next.except]) };
        }
    }
    return result;
}
function admitsValue(admits, value) {
    return 'only' in admits ? admits.only.has(value) : !admits.except.has(value);
}
/** What `step`'s test reads of the incoming message, `undefined` when nothing. */
function readStep(step, isBodyRead) {
    if (step.kind === 'branch')
        return readCondition(step.condition, isBodyRead);
    const subject = step.test.getExpression();
    const read = messageRead(subject, isBodyRead);
    if (!read) {
        const inside = readsIn(subject, isBodyRead);
        return inside.length > 0 ? { kind: 'unread', reads: inside } : undefined;
    }
    // A body after the switch is reached by the values whose clauses run on
    // past it, which this reading does not follow.
    if (step.kind === 'after-switch')
        return { kind: 'unread', reads: [read] };
    const clauses = step.test.getClauses();
    const labels = [];
    for (const clause of clauses) {
        if (!Node.isCaseClause(clause))
            continue;
        const label = stringLiteral(clause.getExpression());
        if (label === undefined)
            return { kind: 'unread', reads: [read] };
        labels.push(label);
    }
    // A clause is entered by its own label and by every clause before it that
    // runs on into it.
    const entered = clauses.map((clause, index) => {
        const group = [];
        for (let at = index; at >= 0; at--) {
            if (at < index && clauses[at].getStatements().some(cannotComplete))
                break;
            const entry = clauses[at];
            group.push(Node.isCaseClause(entry) ? stringLiteral(entry.getExpression()) : null);
        }
        return group;
    });
    return {
        kind: 'values',
        read,
        literals: labels,
        admits: (side) => {
            const index = Number(side.slice('clause:'.length));
            const group = entered[index] ?? [];
            const named = group.filter((label) => label !== null);
            if (group.includes(null)) {
                return { except: new Set(labels.filter((label) => !named.includes(label))) };
            }
            return { only: new Set(named) };
        },
    };
}
/** What an `if` or conditional condition reads of the incoming message. */
function readCondition(condition, isBodyRead) {
    let test = condition;
    let flipped = false;
    for (;;) {
        if (Node.isParenthesizedExpression(test)) {
            test = test.getExpression();
        }
        else if (Node.isPrefixUnaryExpression(test) &&
            test.getOperatorToken() === SyntaxKind.ExclamationToken) {
            flipped = !flipped;
            test = test.getOperand();
        }
        else {
            break;
        }
    }
    if (Node.isBinaryExpression(test)) {
        const operator = test.getOperatorToken().getKind();
        const equal = operator === SyntaxKind.EqualsEqualsEqualsToken || operator === SyntaxKind.EqualsEqualsToken;
        const unequal = operator === SyntaxKind.ExclamationEqualsEqualsToken ||
            operator === SyntaxKind.ExclamationEqualsToken;
        if (equal || unequal) {
            const left = test.getLeft();
            const right = test.getRight();
            const rightLiteral = stringLiteral(right);
            const leftLiteral = stringLiteral(left);
            const value = rightLiteral ?? leftLiteral;
            const operand = rightLiteral !== undefined ? left : right;
            const read = value !== undefined ? messageRead(operand, isBodyRead) : undefined;
            if (read && value !== undefined) {
                // True side admits the value when the test is an equality.
                const trueAdmitsValue = equal !== flipped;
                return {
                    kind: 'values',
                    read,
                    literals: [value],
                    admits: (side) => (side === 'true') === trueAdmitsValue
                        ? { only: new Set([value]) }
                        : { except: new Set([value]) },
                };
            }
        }
    }
    const inside = readsIn(condition, isBodyRead);
    return inside.length > 0 ? { kind: 'unread', reads: inside } : undefined;
}
/** The text of a string literal, `undefined` for anything else. */
export function stringLiteral(node) {
    if (!node)
        return undefined;
    let inner = node;
    while (Node.isParenthesizedExpression(inner))
        inner = inner.getExpression();
    if (Node.isStringLiteral(inner) || Node.isNoSubstitutionTemplateLiteral(inner)) {
        return inner.getLiteralValue();
    }
    return undefined;
}
/** Every message read inside `node`, outermost reads only. */
function readsIn(node, isBodyRead) {
    const found = [];
    for (const candidate of [node, ...node.getDescendants()]) {
        if (found.some((read) => read.node.containsRange(candidate.getPos(), candidate.getEnd())))
            continue;
        const read = messageRead(candidate, isBodyRead);
        if (read)
            found.push(read);
    }
    return found;
}
/**
 * The incoming-message field `node` reads, `undefined` when it is not a read of the
 * incoming message: a chain that starts, by symbol, at a parameter of a function
 * enclosing `node`, through `const` bindings, `await`, parentheses, `as`,
 * `!`, property access, element access with a string literal, a method call
 * whose arguments are all string literals, and `new URL(...)` of the global
 * `URL`. A `let`, a binding default, or a call that takes the message as an
 * argument ends the chain, and it is not a read.
 */
function messageRead(node, isBodyRead) {
    const links = chainOf(node, enclosingParameters(node), 0);
    if (!links || links.length === 0)
        return undefined;
    const field = fieldOf(links);
    if (field === undefined)
        return undefined;
    return { location: locationOf(links, isBodyRead), field, node };
}
function enclosingParameters(node) {
    const parameters = new Set();
    for (const ancestor of node.getAncestors()) {
        if (Node.isFunctionLikeDeclaration(ancestor)) {
            for (const parameter of ancestor.getParameters())
                parameters.add(parameter);
        }
    }
    return parameters;
}
function chainOf(node, roots, depth) {
    if (depth > MAX_CHAIN)
        return undefined;
    const next = (inner) => chainOf(inner, roots, depth + 1);
    if (Node.isParenthesizedExpression(node) ||
        Node.isAsExpression(node) ||
        Node.isNonNullExpression(node)) {
        return next(node.getExpression());
    }
    if (Node.isAwaitExpression(node)) {
        const base = next(node.getExpression());
        return base && [...base, { kind: 'await' }];
    }
    if (Node.isPropertyAccessExpression(node)) {
        const base = next(node.getExpression());
        return base && [...base, { kind: 'property', name: node.getName() }];
    }
    if (Node.isElementAccessExpression(node)) {
        const name = stringLiteral(node.getArgumentExpression());
        if (name === undefined)
            return undefined;
        const base = next(node.getExpression());
        return base && [...base, { kind: 'property', name }];
    }
    if (Node.isCallExpression(node)) {
        const callee = node.getExpression();
        if (!Node.isPropertyAccessExpression(callee))
            return undefined;
        const args = node.getArguments().map((arg) => stringLiteral(arg));
        if (args.some((arg) => arg === undefined))
            return undefined;
        const receiver = callee.getExpression();
        const base = next(receiver);
        return (base && [
            ...base,
            { kind: 'call', name: callee.getName(), args: args, call: node, receiver },
        ]);
    }
    if (Node.isNewExpression(node)) {
        const callee = node.getExpression();
        const [input] = node.getArguments();
        if (!input || !declaredByDefaultLib(callee.getSymbol(), 'URL'))
            return undefined;
        const base = next(input);
        return base && [...base, { kind: 'url' }];
    }
    if (Node.isIdentifier(node)) {
        const declaration = node.getSymbol()?.getDeclarations()[0];
        return declaration ? bindingChain(declaration, roots, depth + 1) : undefined;
    }
    return undefined;
}
/** The chain a declaration binds: an incoming-message parameter, or a `const` read of one. */
function bindingChain(declaration, roots, depth) {
    if (depth > MAX_CHAIN)
        return undefined;
    if (Node.isParameterDeclaration(declaration)) {
        return roots.has(declaration) && !declaration.hasInitializer() ? [] : undefined;
    }
    if (Node.isVariableDeclaration(declaration)) {
        if (!isConst(declaration))
            return undefined;
        const initializer = declaration.getInitializer();
        return initializer ? chainOf(initializer, roots, depth + 1) : undefined;
    }
    if (Node.isBindingElement(declaration)) {
        if (declaration.hasInitializer() || declaration.getDotDotDotToken())
            return undefined;
        const pattern = declaration.getParent();
        if (!Node.isObjectBindingPattern(pattern))
            return undefined;
        const propertyNode = declaration.getPropertyNameNode();
        let name;
        if (!propertyNode) {
            name = declaration.getName();
        }
        else if (Node.isIdentifier(propertyNode)) {
            name = propertyNode.getText();
        }
        else if (Node.isStringLiteral(propertyNode)) {
            name = propertyNode.getLiteralValue();
        }
        if (name === undefined)
            return undefined;
        const holder = pattern.getParent();
        const base = holder ? bindingChain(holder, roots, depth + 1) : undefined;
        return base && [...base, { kind: 'property', name }];
    }
    return undefined;
}
function isConst(declaration) {
    const list = declaration.getParent();
    return (!!list &&
        Node.isVariableDeclarationList(list) &&
        list.getDeclarationKind() === VariableDeclarationKind.Const);
}
/** The last property name or string argument of the chain. */
function fieldOf(links) {
    for (let at = links.length - 1; at >= 0; at--) {
        const link = links[at];
        if (link.kind === 'property')
            return link.name;
        if (link.kind === 'call')
            return link.args.length > 0 ? link.args[link.args.length - 1] : link.name;
    }
    return undefined;
}
function locationOf(links, isBodyRead) {
    const last = links[links.length - 1];
    if (last.kind === 'call' &&
        last.name === 'get' &&
        last.args.length === 1 &&
        declaredByDefaultLib(last.receiver.getType().getSymbol(), 'URLSearchParams')) {
        return 'query';
    }
    if (links.length >= 3 && last.kind === 'property') {
        const awaited = links[links.length - 2];
        const json = links[links.length - 3];
        if (awaited.kind === 'await' &&
            json.kind === 'call' &&
            json.name === 'json' &&
            Node.isCallExpression(json.call) &&
            isBodyRead(json.call)) {
            return 'body';
        }
    }
    return 'unplaced';
}
/**
 * A default lib file declares `symbol` (named `name`, when given). Another
 * runtime's declarations may merge into the same global, so one is enough.
 */
function declaredByDefaultLib(symbol, name) {
    if (!symbol || (name !== undefined && symbol.getName() !== name))
        return false;
    return symbol.getDeclarations().some((declaration) => {
        const file = declaration.getSourceFile();
        return file.getProject().getProgram().compilerObject.isSourceFileDefaultLibrary(file.compilerNode);
    });
}
