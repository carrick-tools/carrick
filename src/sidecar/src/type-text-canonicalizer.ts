/**
 * Canonical union order inside text the COMPILER printed (carrick#775, #760).
 *
 * `expandTypeStructural` walks a type and orders every union it walks
 * (`orderMembers`), so the structural path is stable. But the walk stops at
 * several points and hands the rest of the subtree to `namedText` — the
 * compiler's own print:
 *
 *  - library types, which are kept by name (`isLibraryType`), and whose print
 *    carries their whole generic instantiation with it;
 *  - object types with no named properties (an index signature alone), which
 *    have nothing to walk;
 *  - tuples, functions, cycles the `seen` set breaks, and the depth backstop.
 *
 * Everything inside one of those prints is in the compiler's own union order,
 * which is type-id order, which is CREATION order — the same instability
 * `orderMembers` exists to remove, one level down where the type API can no
 * longer reach it: at that point the members exist only as text.
 *
 * So they are ordered as text, by the same rule the walk uses: every member
 * sorts by its own rendering, compared by UTF-16 code unit. A union is a set,
 * the check phase compares these strings by typechecking them, and a reader
 * diffing two `expanded_definition` strings cannot tell which side of a
 * `namedText` boundary a member came from — so both sides must order them the
 * same way, which is why `orderMembers` no longer keeps intrinsics in compiler
 * id order (the compiler's printer does not use that order either).
 *
 * The rewrite is deliberately timid. Anything whose meaning could depend on
 * order or on a parse this file does not do — a function type (`=>`), a
 * conditional (`?`) — is returned untouched, and untouched is always a legal
 * answer here because the compiler's print is already valid TypeScript.
 */

/** Bracket pairs whose interiors are descended into. */
const CLOSERS: Record<string, string> = { '{': '}', '<': '>', '(': ')', '[': ']' };

/**
 * Put every union in `text` into canonical order, at every nesting level.
 *
 * Returns `text` unchanged whenever the shape is one this file does not parse
 * (see the module comment). Never throws: a type print is arbitrary text, and
 * a definition resolve must not fail because one member confused a scanner.
 */
export function canonicalizeUnionsInText(text: string): string {
  try {
    return canonicalizeType(text);
  } catch {
    return text;
  }
}

/** Canonicalise one TYPE fragment — not a `key: value` member. */
function canonicalizeType(text: string): string {
  const trimmed = text.trim();
  if (trimmed.length === 0) return text;

  // A function type or a conditional type is left exactly as printed: both
  // carry a top-level `|` that is not a union separator at this level
  // (`() => a | b` is one function returning a union, `T extends U ? a : b`
  // has unions inside its branches), and neither is worth the parse.
  if (hasTopLevel(trimmed, ['=>', '?'])) return text;

  const members = splitTopLevel(trimmed, '|');
  if (members.length > 1) {
    return orderTextMembers(members.map((m) => canonicalizeType(m))).join(' | ');
  }

  return descendIntoBrackets(trimmed);
}

/**
 * Rewrite the interiors of the bracket groups in a fragment that is not itself
 * a union: an object body's member types, a generic argument list, a tuple's
 * elements, a parenthesised type.
 */
function descendIntoBrackets(text: string): string {
  let out = '';
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    const quoted = readQuoted(text, i);
    if (quoted !== null) {
      out += text.slice(i, quoted);
      i = quoted;
      continue;
    }
    const closer = CLOSERS[ch];
    if (closer === undefined) {
      out += ch;
      i++;
      continue;
    }
    const end = matchBracket(text, i);
    if (end === -1) {
      // Unbalanced (a truncated print): copy the rest verbatim.
      out += text.slice(i);
      return out;
    }
    const inner = text.slice(i + 1, end);
    out += ch + canonicalizeBody(inner, ch) + closer;
    i = end + 1;
  }
  return out;
}

/**
 * Canonicalise the inside of one bracket group.
 *
 * `{ ... }` is a list of `key: type` members separated by `;` (or `,`), and
 * only the type half may be rewritten — a key is not a type, and `a: X | Y`
 * split on `|` would produce the nonsense member `a: X`. Every other bracket
 * holds a comma-separated list of types.
 *
 * Each piece is spliced back at its own offsets, so a body with nothing to
 * reorder comes out byte-identical to the compiler's print — whitespace,
 * separators and all.
 */
function canonicalizeBody(inner: string, opener: string): string {
  const separator = opener === '{' ? ';' : ',';
  const spans = splitTopLevelSpans(inner, separator);
  let out = '';
  let cursor = 0;
  for (const span of spans) {
    const piece = inner.slice(span.start, span.end);
    const trimmed = piece.trim();
    if (trimmed.length > 0) {
      const rewritten = canonicalizeMember(trimmed);
      if (rewritten !== trimmed) {
        const at = span.start + piece.indexOf(trimmed);
        out += inner.slice(cursor, at) + rewritten;
        cursor = at + trimmed.length;
      }
    }
  }
  return out + inner.slice(cursor);
}

/**
 * One list element: `key: type`, `name?: type`, `[x: string]: type`, or a bare
 * type. Splits at the element's own `:` — the one at depth zero, which an
 * index signature's `[x: string]` and a nested object both sit below — and
 * rewrites only what follows it.
 */
function canonicalizeMember(member: string): string {
  const colon = indexOfTopLevel(member, ':');
  if (colon === -1) return canonicalizeType(member);
  const key = member.slice(0, colon + 1);
  const value = member.slice(colon + 1);
  const leading = value.length - value.trimStart().length;
  const trailing = value.length - value.trimEnd().length;
  return (
    key +
    value.slice(0, leading) +
    canonicalizeType(value.trim()) +
    value.slice(value.length - trailing)
  );
}

/**
 * The canonical order, as text: every member by its own rendering, compared by
 * UTF-16 code unit. This is `orderMembers` in `type-structural-expander.ts`,
 * over the renderings that are all this side has — the two must agree member
 * for member, or one union prints two ways depending on whether the walk or
 * the compiler rendered it.
 */
function orderTextMembers(members: string[]): string[] {
  return members
    .map((text, index) => ({ text: text.trim(), index }))
    .sort((a, b) => {
      if (a.text !== b.text) return a.text < b.text ? -1 : 1;
      return a.index - b.index;
    })
    .map((entry) => entry.text);
}

/** Half-open range of one piece of a depth-zero split. */
interface Span {
  start: number;
  end: number;
}

/**
 * The spans between depth-zero occurrences of `separator`. Depth counts the
 * four bracket pairs; quoted runs are skipped whole, so a separator inside a
 * string literal never splits.
 */
function splitTopLevelSpans(text: string, separator: string): Span[] {
  const spans: Span[] = [];
  let start = 0;
  let depth = 0;
  let i = 0;
  while (i < text.length) {
    const quoted = readQuoted(text, i);
    if (quoted !== null) {
      i = quoted;
      continue;
    }
    const ch = text[i];
    if (CLOSERS[ch] !== undefined) {
      depth++;
    } else if (ch === '}' || ch === ')' || ch === ']' || isTypeArgClose(text, i)) {
      depth = Math.max(0, depth - 1);
    } else if (ch === separator && depth === 0) {
      spans.push({ start, end: i });
      start = i + 1;
    }
    i++;
  }
  spans.push({ start, end: text.length });
  return spans;
}

/** Split at every depth-zero occurrence of `separator`. */
function splitTopLevel(text: string, separator: string): string[] {
  return splitTopLevelSpans(text, separator).map((span) =>
    text.slice(span.start, span.end),
  );
}

/** Index of the first depth-zero `char`, or -1. */
function indexOfTopLevel(text: string, char: string): number {
  const parts = splitTopLevel(text, char);
  return parts.length > 1 ? parts[0].length : -1;
}

/** True when any of `tokens` occurs at depth zero. */
function hasTopLevel(text: string, tokens: string[]): boolean {
  for (const token of tokens) {
    if (token === '=>') {
      // Split on '>' would fight the generic-close rule; look for the arrow
      // directly, at depth zero, by walking with the same scanner.
      if (indexOfTopLevelArrow(text) !== -1) return true;
      continue;
    }
    if (indexOfTopLevel(text, token) !== -1) return true;
  }
  return false;
}

/** Index of a depth-zero `=>`, or -1. */
function indexOfTopLevelArrow(text: string): number {
  let depth = 0;
  let i = 0;
  while (i < text.length) {
    const quoted = readQuoted(text, i);
    if (quoted !== null) {
      i = quoted;
      continue;
    }
    const ch = text[i];
    if (ch === '=' && text[i + 1] === '>') {
      if (depth === 0) return i;
      i += 2;
      continue;
    }
    if (CLOSERS[ch] !== undefined) depth++;
    else if (ch === '}' || ch === ')' || ch === ']' || isTypeArgClose(text, i))
      depth = Math.max(0, depth - 1);
    i++;
  }
  return -1;
}

/** A `>` that closes a type-argument list, rather than the tail of an arrow. */
function isTypeArgClose(text: string, i: number): boolean {
  return text[i] === '>' && text[i - 1] !== '=';
}

/**
 * If a quoted run (string literal or template literal) starts at `i`, the
 * index just past its closing quote; otherwise null. Escapes are honoured so a
 * `"a\"|b"` literal is never split.
 */
function readQuoted(text: string, i: number): number | null {
  const quote = text[i];
  if (quote !== '"' && quote !== "'" && quote !== '`') return null;
  let j = i + 1;
  while (j < text.length) {
    if (text[j] === '\\') {
      j += 2;
      continue;
    }
    if (text[j] === quote) return j + 1;
    j++;
  }
  return text.length;
}

/** Index of the bracket closing the one at `open`, or -1 when unbalanced. */
function matchBracket(text: string, open: number): number {
  const closer = CLOSERS[text[open]];
  let depth = 0;
  let i = open;
  while (i < text.length) {
    const quoted = readQuoted(text, i);
    if (quoted !== null) {
      i = quoted;
      continue;
    }
    const ch = text[i];
    if (ch === text[open]) {
      depth++;
    } else if (ch === closer && !(closer === '>' && text[i - 1] === '=')) {
      depth--;
      if (depth === 0) return i;
    }
    i++;
  }
  return -1;
}
