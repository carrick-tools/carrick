/**
 * The function index's signature text, in an order that does not depend on
 * which process printed it or what that process was asked first
 * (carrick#1993).
 *
 * The compiler prints a union's members in type-id order, and ids are handed
 * out in the order the checker creates types: whichever declaration of a set
 * of members a process meets first decides how every later print of that set
 * reads. So the same slot printed by two processes that met the program's
 * files in different orders can come back `Cat | Dog` from one and
 * `Dog | Cat` from the other. The signature pass is answered by several
 * processes at once, each over its own share of the files, and the text it
 * stores must not depend on the share.
 *
 * The rewrite works on the text alone. The print is parsed by the TypeScript
 * parser (syntax only: no program and no checker), and every union in it, at
 * every depth, is put in the canonical order `orderTextMembers` defines for
 * the rest of the sidecar: each member by its own rewritten text, compared by
 * UTF-16 code unit. Everything else, an intersection's order, a tuple's, a
 * parameter list's, an object's members, is spliced back from the print as it
 * was, byte for byte. Taking the text after the print, rather than building
 * the print another way, means the checker is asked exactly what it was asked
 * before, so nothing a later request in the same process prints can move
 * because of this rewrite.
 *
 * Not yet handled: an object's properties can move the same way. A mapped
 * type over a union of literal keys lists its properties in the order the
 * checker created the keys, and this rewrite leaves an object's members as
 * printed (carrick#1993).
 *
 * It is for the signature kinds only. A contract type (a route's body, a
 * handler's parameter read as a pub/sub payload) keeps the compiler's own
 * print: the capture and the check read those, and they are not this
 * rewrite's to change.
 */

import { ts } from 'ts-morph';
import { canonicalizeUnionsInText, orderTextMembers } from './type-text-canonicalizer.js';

/** The print is parsed as the type of one alias declaration. */
const ALIAS_PREFIX = 'type __signature = ';

/**
 * `text`, a type the compiler printed, with every union in it in canonical
 * order. A print the parser does not read as exactly one type, cleanly, is
 * handed to the text-level canonicaliser the rest of the sidecar uses, which
 * orders what it can and returns the rest untouched. Never throws.
 */
export function stableSignatureText(text: string): string {
  try {
    const type = parsedType(text);
    return type === undefined ? canonicalizeUnionsInText(text) : rewrite(type.node, type.source);
  } catch {
    return canonicalizeUnionsInText(text);
  }
}

/** The type node `text` parses to, when it parses to exactly that and cleanly. */
function parsedType(text: string): { node: ts.TypeNode; source: ts.SourceFile } | undefined {
  const source = ts.createSourceFile(
    'signature.ts',
    `${ALIAS_PREFIX}${text};`,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TS,
  );
  const diagnostics = (source as unknown as { parseDiagnostics?: readonly ts.Diagnostic[] })
    .parseDiagnostics;
  if (diagnostics === undefined || diagnostics.length > 0) return undefined;
  const [statement] = source.statements;
  if (source.statements.length !== 1 || !ts.isTypeAliasDeclaration(statement)) return undefined;
  const node = statement.type;
  // The whole print, and nothing but the print, is the alias's type.
  if (node.getStart(source) !== ALIAS_PREFIX.length) return undefined;
  if (node.end !== ALIAS_PREFIX.length + text.length) return undefined;
  return { node, source };
}

/**
 * `node`'s text with its unions ordered. A union is rebuilt from its members'
 * rewritten texts; any other node is its own text with each child's span
 * replaced by the child's rewrite, so the text between children (brackets,
 * separators, spacing) is the print's own.
 */
function rewrite(node: ts.Node, source: ts.SourceFile): string {
  if (ts.isUnionTypeNode(node)) {
    return orderTextMembers(node.types.map((member) => rewrite(member, source))).join(' | ');
  }
  const text = source.text;
  let out = '';
  let cursor = node.getStart(source);
  ts.forEachChild(node, (child) => {
    const start = child.getStart(source);
    out += text.slice(cursor, start) + rewrite(child, source);
    cursor = child.end;
  });
  return out + text.slice(cursor, node.end);
}

