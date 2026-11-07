/**
 * A route registered once per element of a constant list (carrick#1029).
 *
 * ```ts
 * for (const [path, key] of [['a', 'alpha'], ['b', 'beta']] as const) {
 *   app.get(`/items/${path}`, async () => ({ items: rows()[key] }));
 * }
 * ```
 *
 * One handler serves two operations, `/items/a` and `/items/b`. Typed as
 * written, `key` is `'alpha' | 'beta'`, so each operation's response is the
 * union of both payloads, and a consumer of `/items/a` reads as mismatched.
 *
 * The scanner sends the row's own path (`route_path`) on a route row's
 * request when its line serves more than one operation. This reads, from the
 * syntax and the compiler's symbols only, which element of the list the path
 * names, and returns the rewrite that makes the list that one element. The
 * inferrer then types the same handler in the rewritten file: the compiler
 * infers the response for that element, and no rule of ours decides it.
 *
 * Read only when every piece is stated:
 *
 * - the registration's path argument is a template whose every hole is a name
 *   one loop binds: a `for (const … of LIST)` or `LIST.forEach((…) => …)`
 *   around the registration, binding a name or an array pattern of names;
 * - `LIST` is an array literal (through parentheses, `as const` and
 *   `satisfies`), or a `const` bound to one that nothing in its file mutates,
 *   or that is `as const`;
 * - every element is a string, or an array of strings at the positions the
 *   holes read, with no spread;
 * - exactly one element's path is the row's path, segment by segment from
 *   the end, a parameter matching a parameter of any name.
 *
 * Anything else returns `undefined`, and the request is typed as before.
 */

import { Node, SyntaxKind, type SourceFile, type VariableDeclaration } from 'ts-morph';
import { peelValueWrappers } from './function-body.js';
import { stringLiteral } from './response-modes.js';

/** The rewrite that leaves only the picked element in the list. */
export interface LoopPick {
  file: SourceFile;
  /** The list expression's span in the file's original text. */
  start: number;
  end: number;
  /** What replaces it. Holds no more newlines than the text it replaces. */
  text: string;
  /** Which element, of how many. */
  index: number;
  of: number;
}

/** Methods of an array that change it in place. */
const MUTATORS: ReadonlySet<string> = new Set([
  'push',
  'pop',
  'shift',
  'unshift',
  'splice',
  'sort',
  'reverse',
  'fill',
  'copyWithin',
]);

/** One loop around the registration: what it binds, and its list. */
interface Loop {
  /** Each bound name's node, with the position it reads (`-1`: the whole element). */
  bound: Map<Node, number>;
  list: Node;
}

/**
 * The element of the list `registration` is made once for that `routePath`
 * names, or `undefined` when the code does not state it.
 */
export function loopElementPick(registration: Node, routePath: string): LoopPick | undefined {
  if (!Node.isCallExpression(registration)) return undefined;
  const pathArg = registration.getArguments()[0];
  if (!pathArg) return undefined;
  const template = peelValueWrappers(pathArg);
  if (!Node.isTemplateExpression(template)) return undefined;
  const holes = template.getTemplateSpans().map((span) => span.getExpression());
  if (holes.length === 0 || !holes.every((hole) => Node.isIdentifier(hole))) return undefined;

  const loop = enclosingLoop(registration, holes);
  if (!loop) return undefined;
  const positions = holes.map((hole) => {
    const declared = hole.getSymbol()?.getDeclarations()[0];
    const name = declared && declarationName(declared);
    return name ? loop.bound.get(name) : undefined;
  });
  if (positions.some((position) => position === undefined)) return undefined;

  const elements = listElements(loop.list);
  if (!elements || elements.length < 2) return undefined;
  const values: string[][] = [];
  for (const element of elements) {
    const read = positions.map((position) => elementValue(element, position!));
    if (read.some((value) => value === undefined)) return undefined;
    values.push(read as string[]);
  }

  const head = template.getHead().getLiteralText();
  const tails = template.getTemplateSpans().map((span) => span.getLiteral().getLiteralText());
  const matching = values
    .map((read, index) => ({
      index,
      path: head + read.map((value, at) => value + tails[at]).join(''),
    }))
    .filter(({ path }) => endsWithPath(routePath, path));
  if (matching.length !== 1) return undefined;
  const { index } = matching[0];

  const list = loop.list;
  const original = list.getText();
  const element = elements[index].getText();
  const replacement = `([${element}] as const)`;
  const extraLines = newlines(original) - newlines(replacement);
  if (extraLines < 0) return undefined;
  return {
    file: list.getSourceFile(),
    start: list.getStart(),
    end: list.getEnd(),
    // Padded so every line after the list keeps its number.
    text: `([${element}] as const${'\n'.repeat(extraLines)})`,
    index,
    of: elements.length,
  };
}

/** The name node a declaration binds, for a name declared by a variable, a parameter or a binding element. */
function declarationName(declared: Node): Node | undefined {
  if (
    Node.isVariableDeclaration(declared) ||
    Node.isParameterDeclaration(declared) ||
    Node.isBindingElement(declared)
  ) {
    const name = declared.getNameNode();
    return Node.isIdentifier(name) ? name : undefined;
  }
  return undefined;
}

/** The names a loop's element pattern binds, with the position each reads. */
function boundNames(pattern: Node): Map<Node, number> | undefined {
  if (Node.isIdentifier(pattern)) return new Map([[pattern, -1]]);
  if (!Node.isArrayBindingPattern(pattern)) return undefined;
  const bound = new Map<Node, number>();
  pattern.getElements().forEach((element, position) => {
    if (Node.isOmittedExpression(element)) return;
    const name = element.getNameNode();
    if (element.getDotDotDotToken() || element.getInitializer() || !Node.isIdentifier(name)) return;
    bound.set(name, position);
  });
  return bound;
}

/**
 * The innermost loop around `registration` that binds every hole: a
 * `for (const … of …)` whose body holds it, or a `.forEach` whose callback
 * does.
 */
function enclosingLoop(registration: Node, holes: Node[]): Loop | undefined {
  const bindsAll = (bound: Map<Node, number>) =>
    holes.every((hole) => {
      const declared = hole.getSymbol()?.getDeclarations()[0];
      const name = declared && declarationName(declared);
      return name !== undefined && bound.has(name);
    });
  for (const ancestor of registration.getAncestors()) {
    if (Node.isForOfStatement(ancestor)) {
      const init = ancestor.getInitializer();
      if (!Node.isVariableDeclarationList(init)) continue;
      if (init.getDeclarationKind() !== 'const' || init.getDeclarations().length !== 1) continue;
      const bound = boundNames(init.getDeclarations()[0].getNameNode());
      if (bound && bindsAll(bound)) return { bound, list: ancestor.getExpression() };
      continue;
    }
    if (Node.isArrowFunction(ancestor) || Node.isFunctionExpression(ancestor)) {
      const call = ancestor.getParent();
      if (!Node.isCallExpression(call) || call.getArguments()[0] !== ancestor) continue;
      const callee = call.getExpression();
      if (!Node.isPropertyAccessExpression(callee) || callee.getName() !== 'forEach') continue;
      const param = ancestor.getParameters()[0];
      if (!param || ancestor.getParameters().length > 1) continue;
      const bound = boundNames(param.getNameNode());
      if (bound && bindsAll(bound)) return { bound, list: callee.getExpression() };
    }
  }
  return undefined;
}

/**
 * The elements the list is written with: an array literal, or a `const`
 * holding one that nothing in its file changes. `undefined` for anything
 * else, a spread included.
 */
function listElements(list: Node): Node[] | undefined {
  let literal = peelValueWrappers(list);
  if (Node.isIdentifier(literal)) {
    const declared = literal.getSymbol()?.getDeclarations()[0];
    if (!declared || !Node.isVariableDeclaration(declared)) return undefined;
    const declarationList = declared.getParent();
    if (!Node.isVariableDeclarationList(declarationList) || declarationList.getDeclarationKind() !== 'const') {
      return undefined;
    }
    const initializer = declared.getInitializer();
    if (!initializer) return undefined;
    const readonly = hasConstAssertion(initializer);
    if (!readonly && mutatedInItsFile(declared)) return undefined;
    if (!readonly && declared.isExported()) return undefined;
    literal = peelValueWrappers(initializer);
  }
  if (!Node.isArrayLiteralExpression(literal)) return undefined;
  const elements = literal.getElements();
  if (elements.some((element) => Node.isSpreadElement(element) || Node.isOmittedExpression(element))) {
    return undefined;
  }
  return elements.map((element) => peelValueWrappers(element));
}

function hasConstAssertion(node: Node): boolean {
  let at = node;
  for (;;) {
    if (Node.isAsExpression(at)) {
      if (at.getTypeNode()?.getText() === 'const') return true;
      at = at.getExpression();
    } else if (Node.isParenthesizedExpression(at) || Node.isSatisfiesExpression(at)) {
      at = at.getExpression();
    } else {
      return false;
    }
  }
}

/** Whether a name `declared` binds is assigned, or has a mutating method called, anywhere in its file. */
function mutatedInItsFile(declared: VariableDeclaration): boolean {
  const name = declared.getNameNode();
  const symbol = name.getSymbol();
  if (!symbol) return true;
  for (const ident of declared.getSourceFile().getDescendantsOfKind(SyntaxKind.Identifier)) {
    if (ident.getSymbol() !== symbol || ident === name) continue;
    const parent = ident.getParent();
    if (Node.isBinaryExpression(parent) && parent.getLeft() === ident) return true;
    if (Node.isPropertyAccessExpression(parent) && parent.getExpression() === ident) {
      if (MUTATORS.has(parent.getName())) return true;
      // `LIST.length = 0`, `LIST.x = …`.
      const above = parent.getParent();
      if (Node.isBinaryExpression(above) && above.getLeft() === parent) return true;
    }
    if (Node.isElementAccessExpression(parent) && parent.getExpression() === ident) {
      const above = parent.getParent();
      if (Node.isBinaryExpression(above) && above.getLeft() === parent) return true;
    }
  }
  return false;
}

/** The string an element states at `position` (`-1`: the element itself). */
function elementValue(element: Node, position: number): string | undefined {
  if (position === -1) return stringLiteral(element);
  if (!Node.isArrayLiteralExpression(element)) return undefined;
  const items = element.getElements();
  if (items.some((item) => Node.isSpreadElement(item))) return undefined;
  const item = items[position];
  return item ? stringLiteral(peelValueWrappers(item)) : undefined;
}

/** A path segment that is a parameter, in either spelling. */
function isParam(segment: string): boolean {
  return segment.startsWith(':') || /^\{[^{}]*\}$/.test(segment);
}

/**
 * Whether `full` ends with `own`, segment by segment: the row's path holds
 * the route's own path after whatever prefix its mounts add.
 */
export function endsWithPath(full: string, own: string): boolean {
  const segments = (text: string) => text.split('/').filter((segment) => segment.length > 0);
  const whole = segments(full);
  const tail = segments(own);
  if (tail.length === 0 || tail.length > whole.length) return false;
  const offset = whole.length - tail.length;
  return tail.every((segment, at) => {
    const other = whole[offset + at];
    return isParam(segment) && isParam(other) ? true : segment === other;
  });
}

function newlines(text: string): number {
  return text.split('\n').length - 1;
}
