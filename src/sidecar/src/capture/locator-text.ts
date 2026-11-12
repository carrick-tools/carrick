/**
 * Locator text: the one normaliser the compiler engine's two locators share
 * (carrick#3306), and the mid-text elision rule they share (carrick#3307).
 *
 * A locator is the source text the analyzer quotes for a node. Two matchers
 * read it: the infer path's text match (`TypeInferrer.matchByText`) and the
 * capture's `nodeByExpressionText`. Both compare normalised node text with
 * normalised locator text, so both must normalise alike.
 */

import ts from 'typescript';

/**
 * Source text with every plain string literal written in double quotes:
 * `'/a'`, `"/a"` and an interpolation-free `` `/a` `` are one value, so text
 * that differs only in its quotes compares equal (carrick#2056).
 *
 * The text is read left to right, pairing quotes the way the scanner does,
 * so a template's closing backtick never pairs with the next template's
 * opening one. A literal holding a quote, a backslash or an interpolation is
 * copied as it is, and so is an unterminated one with everything after it.
 */
export function canonicalStringQuotes(text: string): string {
  const isQuote = (ch: string): boolean => ch === "'" || ch === '"' || ch === '`';
  // The index just past the literal opening at `start`, or -1.
  const literalEnd = (start: number): number => {
    const quote = text[start];
    let j = start + 1;
    while (j < text.length) {
      const ch = text[j];
      if (ch === '\\') {
        j += 2;
      } else if (ch === quote) {
        return j + 1;
      } else if (quote === '`' && ch === '$' && text[j + 1] === '{') {
        const close = codeEnd(j + 2);
        if (close < 0) return -1;
        j = close + 1;
      } else {
        j++;
      }
    }
    return -1;
  };
  // The index of the `}` that closes an interpolation whose code starts at `start`, or -1.
  const codeEnd = (start: number): number => {
    let depth = 0;
    let j = start;
    while (j < text.length) {
      const ch = text[j];
      if (isQuote(ch)) {
        const end = literalEnd(j);
        if (end < 0) return -1;
        j = end;
        continue;
      }
      if (ch === '{') depth++;
      if (ch === '}') {
        if (depth === 0) return j;
        depth--;
      }
      j++;
    }
    return -1;
  };

  let out = '';
  let i = 0;
  while (i < text.length) {
    if (!isQuote(text[i])) {
      out += text[i];
      i++;
      continue;
    }
    const end = literalEnd(i);
    if (end < 0) return out + text.slice(i);
    const body = text.slice(i + 1, end - 1);
    out += /['"`\\]|\$\{/.test(body) ? text.slice(i, end) : `"${body}"`;
    i = end;
  }
  return out;
}

/**
 * `text` with its comments replaced by a space, read with the TypeScript
 * scanner so a `//` or `/*` inside a string or template literal stays. Text
 * with no comment marker is returned as it is.
 */
export function stripComments(text: string): string {
  if (!text.includes('//') && !text.includes('/*')) return text;
  const scanner = ts.createScanner(
    ts.ScriptTarget.Latest,
    /* skipTrivia */ false,
    ts.LanguageVariant.Standard,
    text
  );
  // Brace depth per open template expression, so the `}` that ends one
  // resumes the template text instead of code.
  const templates: number[] = [];
  let out = '';
  let token = scanner.scan();
  while (token !== ts.SyntaxKind.EndOfFileToken) {
    if (token === ts.SyntaxKind.OpenBraceToken && templates.length > 0) {
      templates[templates.length - 1]++;
    } else if (token === ts.SyntaxKind.CloseBraceToken && templates.length > 0) {
      if (templates[templates.length - 1] === 0) {
        templates.pop();
        token = scanner.reScanTemplateToken(/* isTaggedTemplate */ false);
        if (token === ts.SyntaxKind.TemplateMiddle) templates.push(0);
      } else {
        templates[templates.length - 1]--;
      }
    } else if (token === ts.SyntaxKind.TemplateHead) {
      templates.push(0);
    }
    out +=
      token === ts.SyntaxKind.SingleLineCommentTrivia ||
      token === ts.SyntaxKind.MultiLineCommentTrivia
        ? ' '
        : scanner.getTokenText();
    token = scanner.scan();
  }
  return out;
}

/**
 * Locator text with insignificant differences removed, so source text and a
 * model's print of it compare equal:
 *
 *  - comments are dropped (carrick#3306);
 *  - whitespace runs collapse to one space;
 *  - a member chain broken before its dot (`client\n  .list(…)`) reads as
 *    `client.list(…)`: a space around `.` / `?.` means nothing (carrick#1162);
 *  - a trailing comma and the space before a closing `)` `]` `}` go (the
 *    model's compact print has neither, #335);
 *  - the space after an opening `(` `[` `{` goes: a multi-line call whose
 *    arguments start on the next line collapses to `f( x)` while the print is
 *    `f(x)` (#336);
 *  - a plain string literal reads the same in any quotes (carrick#2056).
 *
 * Symmetric: apply it to both sides of a comparison.
 */
export function normalizeLocatorText(text: string): string {
  const collapsed = stripComments(text)
    .replace(/\s+/g, ' ')
    .replace(/\s*(\?\.|\.)\s*/g, '$1')
    .replace(/\s*,?\s*([}\)\]])/g, '$1')
    .replace(/([\(\[{])\s+/g, '$1')
    .trim();
  return canonicalStringQuotes(collapsed);
}

/** A locator text split at a mid-text elision (carrick#3307). */
export interface MidElision {
  /** Normalised text before the elision, opener included (`{rows: f(r => {`). */
  prefix: string;
  /** Normalised text after the elision, closer included (`}),total}`). */
  suffix: string;
}

const CLOSER_OF: Record<string, string> = { '(': ')', '[': ']', '{': '}' };
// A mid-text elision names no node in particular below this much text.
const MIN_MID_ELISION_TEXT_LEN = 8;

/**
 * The parts of a NORMALISED locator text around its first mid-text elision:
 * a `...` that sits directly inside a bracket pair (`{ ... }`, `( ... )`,
 * `[ ... ]`) with source text after the pair that the trailing-elision rule
 * does not own. `...rest` is a spread and source text, and an elision at the
 * end of the text is the trailing rule's. Returns `undefined` when there is
 * none.
 */
export function midTextElision(normalized: string): MidElision | undefined {
  let from = 0;
  for (;;) {
    const dots = normalized.indexOf('...', from);
    if (dots < 0) return undefined;
    from = dots + 3;
    const before = normalized.slice(0, dots).replace(/[\s,]*$/, '');
    const opener = before[before.length - 1];
    const closer = CLOSER_OF[opener ?? ''];
    if (closer === undefined || normalized[dots + 3] !== closer) continue;
    const suffix = normalized.slice(dots + 3);
    // Only closers, commas and spaces after the pair: the trailing rule's.
    if (/^[\s,)}\]]*$/.test(suffix)) continue;
    if (before.length + suffix.length < MIN_MID_ELISION_TEXT_LEN) continue;
    return { prefix: before, suffix };
  }
}

/** Whether normalised node text opens with the prefix and ends with the suffix. */
export function matchesMidElision(nodeText: string, elision: MidElision): boolean {
  return (
    nodeText.length >= elision.prefix.length + elision.suffix.length &&
    nodeText.startsWith(elision.prefix) &&
    nodeText.endsWith(elision.suffix)
  );
}
