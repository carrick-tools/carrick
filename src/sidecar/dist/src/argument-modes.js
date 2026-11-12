/**
 * A response read from a function a route calls with its own arguments
 * (carrick#2372).
 *
 * Two routes often share one executor and tell it apart by an argument:
 * `execute(input, 'commit')` for `POST /x` and `execute(input, 'preview')`
 * for `POST /x/preview`, or `read({ id })` for an item route and `read({})`
 * for the list. The executor's sends, or its declared return, are the union
 * of every caller's answers. The compiler gives the same type for every call
 * of a signature that is neither generic nor overloaded, so the route's own
 * answer has to be read from the executor's paths: the tests on a body's
 * path that compare a parameter with a value the route's call states.
 *
 * - **What a call states.** A literal argument (a string, number, boolean,
 *   `null` or `undefined`) states its value; an argument left out states
 *   `undefined` (a parameter with a default states nothing); any other
 *   argument whose type holds neither `null` nor `undefined` states only that
 *   it is defined. A parameter the executor assigns to states nothing, nor
 *   does any parameter of a call with a spread argument. An object literal
 *   argument states each member the same way for a parameter read through a
 *   member access or a destructuring `const` (or a destructured parameter);
 *   a member it leaves out states `undefined` unless the literal spreads
 *   another object.
 * - **What a test decides.** `===`, `==`, `!==` and `!=` between a stated
 *   parameter and a literal, `!`, `&&` and `||`, and the truthiness of a
 *   stated literal. Anything else decides nothing, and a body under a test
 *   that decides nothing is kept.
 * - **Which bodies go.** A body one of whose path tests (an `if` around it,
 *   either arm of a conditional, or an earlier `if` that cannot complete)
 *   decides against the side the body is on. No other body is dropped.
 *
 * `tested` names every stated parameter some test in the executor reads as a
 * mode (a stated literal, or a value stated as defined under a test it
 * decides) and some other call of the executor states differently, whether
 * or not the test decided anything: a union read from such an executor
 * depends on which caller ran it, and where the bodies could not be told
 * apart the caller says so rather than judging the route against every
 * caller's answers. An executor only one call reaches, or whose callers all
 * pass the same value, has no such mode.
 */
import { functionBodyOf, peelValueWrappers } from './function-body.js';
import { Node, SyntaxKind } from 'ts-morph';
import { stepsOnPath } from './failure-path.js';
/** The function a call runs, when it has one body in the program. */
export function calledFunction(call) {
    let symbol = call.getExpression().getSymbol();
    if (symbol?.isAlias())
        symbol = symbol.getAliasedSymbol();
    return functionBodyOf(symbol?.getDeclarations() ?? []);
}
/** The one call in `caller` that runs `callee`, or `undefined` for none or several. */
export function callInto(caller, callee) {
    const calls = caller
        .getDescendantsOfKind(SyntaxKind.CallExpression)
        .filter((call) => calledFunction(call) === callee);
    return calls.length === 1 ? calls[0] : undefined;
}
/**
 * The bodies of `callee` the arguments `call` passes leave reachable. See the
 * module comment for what is read.
 */
export function narrowByArguments(callee, call, bodies) {
    const binding = bindArguments(callee, call);
    if (binding.direct.size === 0 && binding.members.size === 0)
        return { kept: bodies, tested: [] };
    const kept = bodies.filter((body) => stepsOnPath(body, callee).every((step) => {
        if (step.kind !== 'branch')
            return true;
        const decided = evaluate(step.condition, binding);
        return decided === undefined || decided === step.whenTrue;
    }));
    const tested = testedParameters(callee, binding);
    return { kept, tested: tested.length > 0 ? sharedModes(callee, call, binding, tested) : [] };
}
/**
 * The parameters of `tested` some other call of `callee` states differently:
 * another value, or nothing this reading can name. A parameter every call
 * states the same way picks nothing among the callers, so it is no mode.
 */
function sharedModes(callee, call, binding, tested) {
    const name = calleeName(callee);
    if (!name)
        return [];
    const own = statedByName(binding);
    const differing = new Set();
    for (const reference of name.findReferencesAsNodes()) {
        const candidate = reference.getFirstAncestorByKind(SyntaxKind.CallExpression);
        if (!candidate || candidate === call || calledFunction(candidate) !== callee)
            continue;
        const theirs = statedByName(bindArguments(callee, candidate));
        for (const parameter of tested) {
            if (theirs.get(parameter) === undefined || theirs.get(parameter) !== own.get(parameter))
                differing.add(parameter);
        }
    }
    return tested.filter((parameter) => differing.has(parameter));
}
/** The identifier that names `callee` where it is declared. */
function calleeName(callee) {
    if (Node.isFunctionDeclaration(callee))
        return callee.getNameNode();
    if (Node.isMethodDeclaration(callee)) {
        const name = callee.getNameNode();
        return Node.isIdentifier(name) ? name : undefined;
    }
    const holder = callee.getParent();
    if (holder && (Node.isVariableDeclaration(holder) || Node.isPropertyAssignment(holder))) {
        const name = holder.getNameNode();
        return Node.isIdentifier(name) ? name : undefined;
    }
    return undefined;
}
/** Each stated value of a binding, as text, by the name the executor reads it under. */
function statedByName(binding) {
    const byName = new Map();
    const entries = [...binding.direct.values(), ...[...binding.members.values()].flatMap((m) => [...m.values()])];
    for (const entry of entries) {
        byName.set(entry.name, entry.stated.kind === 'defined' ? 'defined' : `value:${String(entry.stated.value)}:${typeof entry.stated.value}`);
    }
    return byName;
}
function bindArguments(callee, call) {
    const binding = { direct: new Map(), members: new Map() };
    const stated = binding.direct;
    if (!Node.isFunctionLikeDeclaration(callee) && !Node.isArrowFunction(callee) && !Node.isFunctionExpression(callee)) {
        return binding;
    }
    const args = call.getArguments();
    if (args.some((arg) => Node.isSpreadElement(arg)))
        return binding;
    const parameters = callee.getParameters();
    parameters.forEach((parameter, index) => {
        if (parameter.isRestParameter())
            return;
        const arg = args[index];
        const name = parameter.getNameNode();
        if (Node.isIdentifier(name)) {
            const symbol = name.getSymbol();
            if (!symbol || assignedIn(callee, symbol))
                return;
            const value = arg === undefined ? (parameter.hasInitializer() ? undefined : UNDEFINED) : statedBy(arg);
            if (value)
                stated.set(symbol, { name: name.getText(), stated: value });
            if (arg === undefined)
                return;
            // Members read through `param.member` or `const { member } = param`.
            const byName = new Map();
            for (const member of memberAccessesOf(callee, symbol)) {
                const memberValue = statedMember(arg, member);
                if (memberValue)
                    byName.set(member, { name: member, stated: memberValue });
            }
            if (byName.size > 0)
                binding.members.set(symbol, byName);
            for (const [member, elementSymbol] of destructuredFrom(callee, symbol)) {
                if (assignedIn(callee, elementSymbol))
                    continue;
                const memberValue = statedMember(arg, member);
                if (memberValue)
                    stated.set(elementSymbol, { name: member, stated: memberValue });
            }
            return;
        }
        if (Node.isObjectBindingPattern(name) && arg !== undefined) {
            for (const element of name.getElements()) {
                if (element.getInitializer() || element.getDotDotDotToken())
                    continue;
                const member = element.getPropertyNameNode()?.getText() ?? element.getName();
                const symbol = element.getNameNode().getSymbol();
                if (!symbol || assignedIn(callee, symbol))
                    continue;
                const memberValue = statedMember(arg, member);
                if (memberValue)
                    stated.set(symbol, { name: member, stated: memberValue });
            }
        }
    });
    return binding;
}
const UNDEFINED = { kind: 'value', value: undefined };
/** What one argument expression states. */
function statedBy(arg) {
    const expression = peelValueWrappers(arg);
    if (Node.isStringLiteral(expression) || Node.isNoSubstitutionTemplateLiteral(expression)) {
        return { kind: 'value', value: expression.getLiteralValue() };
    }
    if (Node.isNumericLiteral(expression))
        return { kind: 'value', value: expression.getLiteralValue() };
    if (expression.getKind() === SyntaxKind.TrueKeyword)
        return { kind: 'value', value: true };
    if (expression.getKind() === SyntaxKind.FalseKeyword)
        return { kind: 'value', value: false };
    if (expression.getKind() === SyntaxKind.NullKeyword)
        return { kind: 'value', value: null };
    if (Node.isIdentifier(expression) && expression.getText() === 'undefined')
        return UNDEFINED;
    const type = expression.getType();
    if (type.isAny() || type.isUnknown())
        return undefined;
    const members = type.isUnion() ? type.getUnionTypes() : [type];
    if (members.some((member) => member.isNull() || member.isUndefined() || member.isVoid()))
        return undefined;
    return { kind: 'defined' };
}
/** What an argument states about one of its members, when it is an object literal. */
function statedMember(arg, member) {
    const literal = peelValueWrappers(arg);
    if (!Node.isObjectLiteralExpression(literal))
        return undefined;
    let found;
    let spread = false;
    for (const property of literal.getProperties()) {
        if (Node.isSpreadAssignment(property)) {
            spread = true;
            continue;
        }
        if (Node.isPropertyAssignment(property) && property.getNameNode().getText() === member) {
            if (Node.isComputedPropertyName(property.getNameNode()))
                return undefined;
            found = statedBy(property.getInitializerOrThrow());
        }
        else if (Node.isShorthandPropertyAssignment(property) && property.getName() === member) {
            found = statedBy(property.getNameNode());
        }
        else if ((Node.isPropertyAssignment(property) && Node.isComputedPropertyName(property.getNameNode())) ||
            Node.isGetAccessorDeclaration(property)) {
            // A member this reading cannot name could be the one asked for.
            if (found === undefined)
                spread = true;
        }
    }
    // A later spread could overwrite an earlier member, and could hold one left out.
    if (spread)
        return undefined;
    return found ?? UNDEFINED;
}
/** The bindings a destructuring `const` of the parameter `symbol` declares, by member name. */
function destructuredFrom(callee, symbol) {
    const read = [];
    for (const declaration of callee.getDescendantsOfKind(SyntaxKind.VariableDeclaration)) {
        const initializer = declaration.getInitializer();
        const pattern = declaration.getNameNode();
        if (!initializer || !Node.isIdentifier(initializer) || initializer.getSymbol() !== symbol)
            continue;
        if (!Node.isObjectBindingPattern(pattern))
            continue;
        const list = declaration.getParent();
        if (!list || !Node.isVariableDeclarationList(list) || list.getDeclarationKind() !== 'const')
            continue;
        for (const element of pattern.getElements()) {
            if (element.getInitializer() || element.getDotDotDotToken())
                continue;
            const name = element.getNameNode();
            if (!Node.isIdentifier(name))
                continue;
            const elementSymbol = name.getSymbol();
            if (elementSymbol)
                read.push([element.getPropertyNameNode()?.getText() ?? element.getName(), elementSymbol]);
        }
    }
    return read;
}
/** The member names read as `param.member` on the parameter `symbol`; none when any is written. */
function memberAccessesOf(callee, symbol) {
    const names = new Set();
    for (const access of callee.getDescendantsOfKind(SyntaxKind.PropertyAccessExpression)) {
        const object = access.getExpression();
        if (!Node.isIdentifier(object) || object.getSymbol() !== symbol)
            continue;
        if (isWritten(access))
            return [];
        names.add(access.getName());
    }
    return [...names];
}
/** True when `symbol` is assigned to, or updated, anywhere in `scope`. */
function assignedIn(scope, symbol) {
    return scope
        .getDescendantsOfKind(SyntaxKind.Identifier)
        .some((identifier) => identifier.getSymbol() === symbol && isWritten(identifier));
}
function isWritten(node) {
    const parent = node.getParent();
    if (!parent)
        return false;
    if (Node.isBinaryExpression(parent) && parent.getLeft() === node) {
        const operator = parent.getOperatorToken().getKind();
        return operator >= SyntaxKind.FirstAssignment && operator <= SyntaxKind.LastAssignment;
    }
    if (Node.isPrefixUnaryExpression(parent) || Node.isPostfixUnaryExpression(parent)) {
        const operator = parent.getOperatorToken();
        return operator === SyntaxKind.PlusPlusToken || operator === SyntaxKind.MinusMinusToken;
    }
    return false;
}
/** The entry a test operand reads, when it reads a stated value. */
function entryOf(node, binding) {
    const operand = peelValueWrappers(node);
    if (Node.isIdentifier(operand)) {
        const symbol = operand.getSymbol();
        return symbol ? binding.direct.get(symbol) : undefined;
    }
    if (Node.isPropertyAccessExpression(operand)) {
        const object = peelValueWrappers(operand.getExpression());
        const symbol = Node.isIdentifier(object) ? object.getSymbol() : undefined;
        return symbol ? binding.members.get(symbol)?.get(operand.getName()) : undefined;
    }
    return undefined;
}
function statedOperand(node, binding) {
    return entryOf(node, binding)?.stated;
}
/** The literal a test operand is, when it is one. */
function literalOperand(node) {
    const operand = peelValueWrappers(node);
    if (Node.isVoidExpression(operand))
        return UNDEFINED;
    if (Node.isStringLiteral(operand) ||
        Node.isNoSubstitutionTemplateLiteral(operand) ||
        Node.isNumericLiteral(operand) ||
        operand.getKind() === SyntaxKind.TrueKeyword ||
        operand.getKind() === SyntaxKind.FalseKeyword ||
        operand.getKind() === SyntaxKind.NullKeyword ||
        (Node.isIdentifier(operand) && operand.getText() === 'undefined')) {
        return statedBy(operand);
    }
    return undefined;
}
/** What a stated value compared with a literal gives, or `undefined` when undecided. */
function compare(stated, literal, loose) {
    if (literal.kind !== 'value')
        return undefined;
    if (stated.kind === 'defined') {
        // Defined means neither null nor undefined; any other literal could match.
        return literal.value === null || literal.value === undefined ? false : undefined;
    }
    if (loose && (stated.value === null || stated.value === undefined)) {
        return literal.value === null || literal.value === undefined;
    }
    return stated.value === literal.value;
}
/** True, false, or `undefined` when the stated arguments do not decide `condition`. */
function evaluate(condition, binding) {
    const test = peelValueWrappers(condition);
    if (Node.isPrefixUnaryExpression(test) && test.getOperatorToken() === SyntaxKind.ExclamationToken) {
        const inner = evaluate(test.getOperand(), binding);
        return inner === undefined ? undefined : !inner;
    }
    if (Node.isBinaryExpression(test)) {
        const operator = test.getOperatorToken().getKind();
        if (operator === SyntaxKind.AmpersandAmpersandToken || operator === SyntaxKind.BarBarToken) {
            const left = evaluate(test.getLeft(), binding);
            const right = evaluate(test.getRight(), binding);
            if (operator === SyntaxKind.AmpersandAmpersandToken) {
                if (left === false || right === false)
                    return false;
                return left === true && right === true ? true : undefined;
            }
            if (left === true || right === true)
                return true;
            return left === false && right === false ? false : undefined;
        }
        const equality = operator === SyntaxKind.EqualsEqualsEqualsToken || operator === SyntaxKind.EqualsEqualsToken;
        const inequality = operator === SyntaxKind.ExclamationEqualsEqualsToken || operator === SyntaxKind.ExclamationEqualsToken;
        if (!equality && !inequality)
            return undefined;
        const loose = operator === SyntaxKind.EqualsEqualsToken || operator === SyntaxKind.ExclamationEqualsToken;
        const pairs = [
            [test.getLeft(), test.getRight()],
            [test.getRight(), test.getLeft()],
        ];
        for (const [maybeStated, maybeLiteral] of pairs) {
            const stated = statedOperand(maybeStated, binding);
            const literal = literalOperand(maybeLiteral);
            if (!stated || !literal)
                continue;
            const equal = compare(stated, literal, loose);
            if (equal === undefined)
                return undefined;
            return equality ? equal : !equal;
        }
        return undefined;
    }
    const stated = statedOperand(test, binding);
    if (stated?.kind === 'value')
        return Boolean(stated.value);
    return undefined;
}
/**
 * Every stated parameter a test anywhere in `callee` reads as a mode: a
 * stated literal under any test, or a value stated as defined under a test
 * that value decides.
 */
function testedParameters(callee, binding) {
    const names = new Set();
    const conditions = [];
    callee.forEachDescendant((node) => {
        if (Node.isIfStatement(node))
            conditions.push(node.getExpression());
        else if (Node.isConditionalExpression(node))
            conditions.push(node.getCondition());
        else if (Node.isSwitchStatement(node))
            conditions.push(node.getExpression());
    });
    for (const condition of conditions) {
        for (const node of [condition, ...condition.getDescendants()]) {
            // A stated literal is a mode wherever a test reads it; a value stated
            // only as defined is one where a test it decides reads it.
            const entry = entryOf(node, binding);
            if (entry?.stated.kind === 'value')
                names.add(entry.name);
            if (Node.isBinaryExpression(node) && evaluate(node, binding) !== undefined) {
                for (const operand of [node.getLeft(), node.getRight()]) {
                    const compared = entryOf(operand, binding);
                    if (compared)
                        names.add(compared.name);
                }
            }
        }
    }
    return [...names].sort();
}
