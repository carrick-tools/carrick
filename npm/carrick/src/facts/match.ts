// A light declaration scanner: which named things an edit touched, read from
// the file's current text with no compiler.
//
// The fact file keys a function by the name the naming rule gives it (the
// seam doc in carrick-cloud, `docs/internal/reference/edit-facts-wire.md`,
// "Naming rule for `name`"): `f` for a declared or const-bound function,
// `Class.method` for a class member, `obj.Key.key` for a function inside an
// object literal bound to a const, and the nearest named encloser for an
// anonymous one. This finds the same names from the text as it is now, so a
// function that moved on the agent's branch is still found, and a renamed one
// is not.
//
// One tokenizer pass (strings, templates, comments and regex literals are
// skipped), then a walk over matched brackets. Whenever the text does not
// read cleanly (an unbalanced bracket, an unterminated string), the scan says
// so and the caller prints nothing for the file.

type TokenKind = "id" | "str" | "tpl" | "num" | "punc" | "regex";

type Token = { kind: TokenKind; value: string; start: number; end: number; line: number };

/** A named function-like thing and the text span it covers. */
export type NamedSpan = { name: string; start: number; end: number };

/** A call's argument list and the string literals passed to it directly. */
export type CallSpan = { start: number; end: number; literals: string[] };

export type Scan = {
  /** False when the text did not tokenize cleanly; nothing else is set then. */
  ok: boolean;
  spans: NamedSpan[];
  calls: CallSpan[];
};

const OPEN: Record<string, string> = { "(": ")", "[": "]", "{": "}" };
const CLOSE = new Set([")", "]", "}"]);
/** After these a `/` starts a regex literal, not a division. */
const REGEX_AFTER_PUNC = new Set([
  "(", ",", "=", ":", "[", "!", "&", "|", "?", "{", "}", ";", "+", "-", "*", "%", "<", ">", "~", "^", "=>",
  "==", "===", "!=", "!==", "&&", "||", "??", "+=", "-=", "*=", "/=", "<=", ">=", "...",
]);
const REGEX_AFTER_WORD = new Set([
  "return", "typeof", "case", "do", "else", "in", "of", "new", "delete", "void", "throw", "yield", "await", "instanceof",
]);
const PUNCS = ["...", "===", "!==", "**=", "??=", "&&=", "||=", "=>", "==", "!=", "<=", ">=", "&&", "||", "??", "?.", "+=", "-=", "*=", "/=", "%=", "++", "--"];
const CLASS_MODIFIERS = new Set([
  "static", "async", "get", "set", "public", "private", "protected", "readonly", "override", "abstract", "declare", "accessor",
]);

function isIdStart(ch: string): boolean {
  return /[A-Za-z_$#]/.test(ch) || ch.charCodeAt(0) > 127;
}

function isIdPart(ch: string): boolean {
  return /[A-Za-z0-9_$]/.test(ch) || ch.charCodeAt(0) > 127;
}

/** The tokens of `text`, or null when a string, template, comment or regex does not end. */
export function tokenize(text: string): Token[] | null {
  const tokens: Token[] = [];
  let i = 0;
  let line = 1;
  const n = text.length;
  const prevSignificant = (): Token | undefined => tokens[tokens.length - 1];

  // Skips a template literal starting at the backtick at `at`; returns the
  // index after it and whether it had substitutions, or null if unterminated.
  const skipTemplate = (at: number): { end: number; plain: boolean } | null => {
    let k = at + 1;
    let plain = true;
    while (k < n) {
      const ch = text[k]!;
      if (ch === "\\") {
        k += 2;
        continue;
      }
      if (ch === "\n") line += 1;
      if (ch === "`") return { end: k + 1, plain };
      if (ch === "$" && text[k + 1] === "{") {
        plain = false;
        k += 2;
        let depth = 1;
        while (k < n && depth > 0) {
          const c = text[k]!;
          if (c === "\n") line += 1;
          if (c === "'" || c === '"') {
            const close = skipString(k);
            if (close === null) return null;
            k = close;
            continue;
          }
          if (c === "`") {
            const inner = skipTemplate(k);
            if (inner === null) return null;
            k = inner.end;
            continue;
          }
          if (c === "{") depth += 1;
          else if (c === "}") depth -= 1;
          k += 1;
        }
        if (depth > 0) return null;
        continue;
      }
      k += 1;
    }
    return null;
  };

  // The index after the quoted string starting at `at`, or null if unterminated.
  const skipString = (at: number): number | null => {
    const quote = text[at]!;
    let k = at + 1;
    while (k < n) {
      const ch = text[k]!;
      if (ch === "\\") {
        k += 2;
        continue;
      }
      if (ch === quote) return k + 1;
      if (ch === "\n") return null;
      k += 1;
    }
    return null;
  };

  while (i < n) {
    const ch = text[i]!;
    if (ch === "\n") {
      line += 1;
      i += 1;
      continue;
    }
    if (ch === " " || ch === "\t" || ch === "\r" || ch === "\f" || ch === "\v" || ch === " " || ch === "﻿") {
      i += 1;
      continue;
    }
    if (ch === "/" && text[i + 1] === "/") {
      while (i < n && text[i] !== "\n") i += 1;
      continue;
    }
    if (ch === "/" && text[i + 1] === "*") {
      const close = text.indexOf("*/", i + 2);
      if (close < 0) return null;
      for (let k = i; k < close; k += 1) if (text[k] === "\n") line += 1;
      i = close + 2;
      continue;
    }
    const startLine = line;
    if (ch === "'" || ch === '"') {
      const close = skipString(i);
      if (close === null) return null;
      tokens.push({ kind: "str", value: unquote(text.slice(i + 1, close - 1)), start: i, end: close, line: startLine });
      i = close;
      continue;
    }
    if (ch === "`") {
      const done = skipTemplate(i);
      if (done === null) return null;
      tokens.push({
        kind: done.plain ? "str" : "tpl",
        value: done.plain ? unquote(text.slice(i + 1, done.end - 1)) : "",
        start: i,
        end: done.end,
        line: startLine,
      });
      i = done.end;
      continue;
    }
    if (ch === "/") {
      const prev = prevSignificant();
      const regex =
        prev === undefined ||
        (prev.kind === "punc" && REGEX_AFTER_PUNC.has(prev.value)) ||
        (prev.kind === "id" && REGEX_AFTER_WORD.has(prev.value));
      if (regex) {
        let k = i + 1;
        let inClass = false;
        while (k < n) {
          const c = text[k]!;
          if (c === "\n") return null;
          if (c === "\\") {
            k += 2;
            continue;
          }
          if (c === "[") inClass = true;
          else if (c === "]") inClass = false;
          else if (c === "/" && !inClass) break;
          k += 1;
        }
        if (k >= n) return null;
        k += 1;
        while (k < n && /[A-Za-z]/.test(text[k]!)) k += 1;
        tokens.push({ kind: "regex", value: "", start: i, end: k, line: startLine });
        i = k;
        continue;
      }
    }
    if (isIdStart(ch)) {
      let k = i + 1;
      while (k < n && isIdPart(text[k]!)) k += 1;
      tokens.push({ kind: "id", value: text.slice(i, k), start: i, end: k, line: startLine });
      i = k;
      continue;
    }
    if (/[0-9]/.test(ch) || (ch === "." && /[0-9]/.test(text[i + 1] ?? ""))) {
      let k = i + 1;
      while (k < n && /[0-9A-Za-z_.]/.test(text[k]!)) k += 1;
      tokens.push({ kind: "num", value: text.slice(i, k), start: i, end: k, line: startLine });
      i = k;
      continue;
    }
    const multi = PUNCS.find((p) => text.startsWith(p, i));
    const value = multi ?? ch;
    tokens.push({ kind: "punc", value, start: i, end: i + value.length, line: startLine });
    i += value.length;
  }
  return tokens;
}

function unquote(body: string): string {
  return body.replace(/\\(.)/g, "$1");
}

/** For every bracket token, the index of its partner; null when they do not balance. */
function matchBrackets(tokens: Token[]): Map<number, number> | null {
  const match = new Map<number, number>();
  const stack: number[] = [];
  for (const [index, token] of tokens.entries()) {
    if (token.kind !== "punc") continue;
    if (OPEN[token.value]) stack.push(index);
    else if (CLOSE.has(token.value)) {
      const open = stack.pop();
      if (open === undefined || OPEN[tokens[open]!.value] !== token.value) return null;
      match.set(open, index);
      match.set(index, open);
    }
  }
  return stack.length === 0 ? match : null;
}

type Ctx = { kind: "code" } | { kind: "class"; name: string } | { kind: "object"; path: string };

/** Scan `text` for named spans and calls. */
export function scanSource(text: string): Scan {
  const tokens = tokenize(text);
  const match = tokens ? matchBrackets(tokens) : null;
  if (!tokens || !match) return { ok: false, spans: [], calls: [] };
  const spans: NamedSpan[] = [];
  const calls: CallSpan[] = [];
  const t = (index: number): Token | undefined => tokens[index];
  const is = (index: number, kind: TokenKind, value?: string): boolean => {
    const token = t(index);
    return token !== undefined && token.kind === kind && (value === undefined || token.value === value);
  };
  const punc = (index: number, value: string): boolean => is(index, "punc", value);

  // Every call's direct string arguments.
  for (const [index, token] of tokens.entries()) {
    if (token.kind !== "punc" || token.value !== "(") continue;
    const prev = t(index - 1);
    const callee =
      prev !== undefined &&
      ((prev.kind === "id" && !REGEX_AFTER_WORD.has(prev.value) && prev.value !== "function" && prev.value !== "if" &&
        prev.value !== "for" && prev.value !== "while" && prev.value !== "switch" && prev.value !== "catch") ||
        (prev.kind === "punc" && (prev.value === ")" || prev.value === "]" || prev.value === "?.")) ||
        prev.kind === "tpl");
    if (!callee) continue;
    const close = match.get(index)!;
    const literals: string[] = [];
    for (let k = index + 1; k < close; k += 1) {
      const inner = t(k)!;
      if (inner.kind === "punc" && OPEN[inner.value]) {
        k = match.get(k)!;
        continue;
      }
      if (inner.kind === "str") literals.push(inner.value);
    }
    calls.push({ start: token.start, end: tokens[close]!.end, literals });
  }

  /** Skips a type annotation after `:` up to the token where `stop` holds, jumping brackets. */
  const skipType = (from: number, limit: number, stop: (index: number) => boolean): number => {
    let k = from;
    while (k < limit && !stop(k)) {
      const token = t(k)!;
      if (token.kind === "punc" && OPEN[token.value]) {
        k = match.get(k)! + 1;
        continue;
      }
      if (token.kind === "punc" && (token.value === ";" || CLOSE.has(token.value))) return k;
      k += 1;
    }
    return k;
  };

  /** The index of the body `{` after a parameter list closing at `close`, or -1. */
  const bodyAfterParams = (close: number, limit: number): number => {
    let k = close + 1;
    if (punc(k, ":")) {
      k += 1;
      while (k < limit) {
        const token = t(k)!;
        if (token.kind === "punc" && token.value === "{") {
          const before = t(k - 1)!;
          const typeOpener = before.kind === "punc" && [":", "|", "&", "<", ",", "(", "=>"].includes(before.value);
          if (!typeOpener) return k;
          k = match.get(k)! + 1;
          continue;
        }
        if (token.kind === "punc" && OPEN[token.value]) {
          k = match.get(k)! + 1;
          continue;
        }
        if (token.kind === "punc" && (token.value === ";" || CLOSE.has(token.value))) return -1;
        k += 1;
      }
      return -1;
    }
    return punc(k, "{") ? k : -1;
  };

  /** The end (exclusive token index) of an expression starting at `from`. */
  const exprEnd = (from: number, limit: number): number => {
    let k = from;
    while (k < limit) {
      const token = t(k)!;
      if (token.kind === "punc" && OPEN[token.value]) {
        k = match.get(k)! + 1;
        continue;
      }
      if (token.kind === "punc" && (token.value === "," || token.value === ";" || CLOSE.has(token.value))) return k;
      if (k > from) {
        const prev = t(k - 1)!;
        const prevEnds =
          prev.kind === "str" || prev.kind === "num" || prev.kind === "tpl" || prev.kind === "regex" ||
          (prev.kind === "id" && !REGEX_AFTER_WORD.has(prev.value)) ||
          (prev.kind === "punc" && (prev.value === ")" || prev.value === "]" || prev.value === "}"));
        const startsStatement =
          (token.kind === "id" && !["as", "satisfies", "instanceof", "in", "of"].includes(token.value)) ||
          token.kind === "str";
        if (token.line > prev.line && prevEnds && startsStatement) return k;
      }
      k += 1;
    }
    return limit;
  };

  /**
   * Whether a function expression starts at `at`; when it does, the token
   * range of its body (`bodyOpen` set for a block body) and the index after it.
   */
  const functionAt = (at: number, limit: number): { bodyOpen: number; after: number; exprFrom: number } | null => {
    let k = at;
    if (is(k, "id", "async") && !punc(k + 1, "=>") && !punc(k + 1, "=")) k += 1;
    if (is(k, "id", "function")) {
      k += 1;
      if (punc(k, "*")) k += 1;
      if (is(k, "id")) k += 1;
      if (!punc(k, "(")) return null;
      const body = bodyAfterParams(match.get(k)!, limit);
      if (body < 0) return null;
      return { bodyOpen: body, after: match.get(body)! + 1, exprFrom: -1 };
    }
    let arrow = -1;
    if (is(k, "id") && punc(k + 1, "=>")) arrow = k + 1;
    else if (punc(k, "(")) {
      const close = match.get(k)!;
      if (punc(close + 1, "=>")) arrow = close + 1;
      else if (punc(close + 1, ":")) {
        const at2 = skipType(close + 2, limit, (index) => punc(index, "=>"));
        if (punc(at2, "=>")) arrow = at2;
      }
    }
    if (arrow < 0) return null;
    if (punc(arrow + 1, "{")) return { bodyOpen: arrow + 1, after: match.get(arrow + 1)! + 1, exprFrom: -1 };
    const end = exprEnd(arrow + 1, limit);
    return { bodyOpen: -1, after: end, exprFrom: arrow + 1 };
  };

  /** Records a named function and walks its body. Returns the index after it. */
  const named = (name: string, declStart: number, fn: { bodyOpen: number; after: number; exprFrom: number }): number => {
    const last = t(fn.after - 1)!;
    spans.push({ name, start: tokens[declStart]!.start, end: last.end });
    if (fn.bodyOpen >= 0) walk(fn.bodyOpen + 1, match.get(fn.bodyOpen)!, { kind: "code" });
    else walk(fn.exprFrom, fn.after, { kind: "code" });
    return fn.after;
  };

  const walkCode = (from: number, limit: number): void => {
    let i = from;
    while (i < limit) {
      const token = t(i)!;
      if (token.kind === "id" && token.value === "function") {
        let k = i + 1;
        if (punc(k, "*")) k += 1;
        if (is(k, "id") && punc(k + 1, "(")) {
          const fn = functionAt(i, limit);
          if (fn) {
            i = named(t(k)!.value, i, fn);
            continue;
          }
        }
      }
      if (token.kind === "id" && (token.value === "const" || token.value === "let" || token.value === "var") && is(i + 1, "id")) {
        const name = t(i + 1)!.value;
        let k = i + 2;
        if (punc(k, ":")) k = skipType(k + 1, limit, (index) => punc(index, "="));
        if (punc(k, "=")) {
          const value = k + 1;
          if (punc(value, "{")) {
            walk(value + 1, match.get(value)!, { kind: "object", path: name });
            i = match.get(value)! + 1;
            continue;
          }
          const fn = functionAt(value, limit);
          if (fn) {
            i = named(name, i, fn);
            continue;
          }
          i = value;
          continue;
        }
      }
      if (token.kind === "id" && token.value === "class" && is(i + 1, "id")) {
        const name = t(i + 1)!.value;
        let k = i + 2;
        while (k < limit && !punc(k, "{")) {
          k = punc(k, "(") || punc(k, "[") ? match.get(k)! + 1 : k + 1;
        }
        if (k < limit) {
          walk(k + 1, match.get(k)!, { kind: "class", name });
          i = match.get(k)! + 1;
          continue;
        }
      }
      if (token.kind === "punc" && OPEN[token.value]) {
        walk(i + 1, match.get(i)!, { kind: "code" });
        i = match.get(i)! + 1;
        continue;
      }
      i += 1;
    }
  };

  /** The index of the next `,` (or `;` in a class) at this level, walking nested brackets as code. */
  const nextEntry = (from: number, limit: number, separators: string[]): number => {
    let k = from;
    while (k < limit) {
      const token = t(k)!;
      if (token.kind === "punc" && separators.includes(token.value)) return k + 1;
      if (token.kind === "punc" && OPEN[token.value]) {
        walk(k + 1, match.get(k)!, { kind: "code" });
        k = match.get(k)! + 1;
        continue;
      }
      k += 1;
    }
    return limit;
  };

  const walkObject = (from: number, limit: number, path: string): void => {
    let i = from;
    while (i < limit) {
      if (punc(i, "...")) {
        i = nextEntry(i, limit, [","]);
        continue;
      }
      let k = i;
      while ((is(k, "id", "async") || is(k, "id", "get") || is(k, "id", "set")) && !punc(k + 1, ":") && !punc(k + 1, "(") && !punc(k + 1, ",")) {
        k += 1;
      }
      if (punc(k, "*")) k += 1;
      let key: string | null = null;
      if (is(k, "id") || is(k, "str") || is(k, "num")) {
        key = t(k)!.value;
        k += 1;
      } else if (punc(k, "[")) {
        k = match.get(k)! + 1;
      } else {
        i = nextEntry(i, limit, [","]);
        continue;
      }
      if (punc(k, "?")) k += 1;
      if (punc(k, "(")) {
        const body = bodyAfterParams(match.get(k)!, limit);
        if (body >= 0) {
          if (key !== null) named(`${path}.${key}`, i, { bodyOpen: body, after: match.get(body)! + 1, exprFrom: -1 });
          else walk(body + 1, match.get(body)!, { kind: "code" });
          i = nextEntry(match.get(body)! + 1, limit, [","]);
          continue;
        }
      }
      if (punc(k, ":")) {
        const value = k + 1;
        if (punc(value, "{") && key !== null) {
          walk(value + 1, match.get(value)!, { kind: "object", path: `${path}.${key}` });
          i = nextEntry(match.get(value)! + 1, limit, [","]);
          continue;
        }
        const fn = functionAt(value, limit);
        if (fn && key !== null) {
          const after = named(`${path}.${key}`, i, fn);
          i = nextEntry(after, limit, [","]);
          continue;
        }
        i = nextEntry(value, limit, [","]);
        continue;
      }
      i = nextEntry(k, limit, [","]);
    }
  };

  const walkClass = (from: number, limit: number, className: string): void => {
    let i = from;
    while (i < limit) {
      if (punc(i, ";")) {
        i += 1;
        continue;
      }
      const memberStart = i;
      let k = i;
      // Decorators: `@name`, `@a.b`, `@name(...)`.
      while (punc(k, "@")) {
        k += 1;
        while (is(k, "id") || punc(k, ".")) k += 1;
        if (punc(k, "(")) k = match.get(k)! + 1;
      }
      while (is(k, "id") && CLASS_MODIFIERS.has(t(k)!.value) && !punc(k + 1, "(") && !punc(k + 1, "=") && !punc(k + 1, ";") && !punc(k + 1, ":")) {
        k += 1;
      }
      if (punc(k, "*")) k += 1;
      if (punc(k, "{")) {
        // A static block.
        walk(k + 1, match.get(k)!, { kind: "code" });
        i = match.get(k)! + 1;
        continue;
      }
      let key: string | null = null;
      if (is(k, "id") || is(k, "str") || is(k, "num")) {
        key = t(k)!.value;
        k += 1;
      } else if (punc(k, "[")) {
        k = match.get(k)! + 1;
      } else {
        i = k + 1;
        continue;
      }
      if (punc(k, "?") || punc(k, "!")) k += 1;
      if (punc(k, "(")) {
        const body = bodyAfterParams(match.get(k)!, limit);
        if (body >= 0) {
          if (key !== null) named(`${className}.${key}`, memberStart, { bodyOpen: body, after: match.get(body)! + 1, exprFrom: -1 });
          else walk(body + 1, match.get(body)!, { kind: "code" });
          i = match.get(body)! + 1;
          continue;
        }
        // An overload or abstract signature.
        i = nextEntry(match.get(k)! + 1, limit, [";"]);
        continue;
      }
      if (punc(k, ":")) k = skipType(k + 1, limit, (index) => punc(index, "=") || punc(index, ";") || (t(index)!.line > t(index - 1)!.line && is(index, "id")));
      if (punc(k, "=")) {
        const fn = functionAt(k + 1, limit);
        if (fn && key !== null) {
          i = named(`${className}.${key}`, memberStart, fn);
          continue;
        }
        const end = exprEnd(k + 1, limit);
        walk(k + 1, end, { kind: "code" });
        i = end;
        continue;
      }
      i = k;
    }
  };

  function walk(from: number, limit: number, ctx: Ctx): void {
    if (ctx.kind === "code") walkCode(from, limit);
    else if (ctx.kind === "object") walkObject(from, limit, ctx.path);
    else walkClass(from, limit, ctx.name);
  }

  walk(0, tokens.length, { kind: "code" });
  return { ok: true, spans, calls };
}

/** A changed region of the file, as character offsets (end exclusive). */
export type Range = { start: number; end: number };

/** Where `snippets` sit in `text` (every occurrence). Empty when none is found. */
export function rangesOf(text: string, snippets: readonly string[]): Range[] {
  const ranges: Range[] = [];
  for (const snippet of snippets) {
    if (snippet.trim().length === 0) continue;
    let at = text.indexOf(snippet);
    while (at >= 0) {
      ranges.push({ start: at, end: at + snippet.length });
      at = text.indexOf(snippet, at + snippet.length);
    }
  }
  return ranges;
}

/** The innermost named span holding `offset`, or null at the top level. */
function innermost(spans: readonly NamedSpan[], offset: number): NamedSpan | null {
  let best: NamedSpan | null = null;
  for (const span of spans) {
    if (span.start <= offset && offset < span.end && (best === null || span.end - span.start < best.end - best.start)) best = span;
  }
  return best;
}

/** What an edit touched: the named things, and the unnamed calls it reached. */
export type Touched = { names: string[]; calls: CallSpan[] };

/**
 * The names an edit over `ranges` touched (the innermost named encloser of
 * each changed line, and every named thing inside a changed range), and the
 * calls outside every named thing that overlap a changed range.
 */
export function touched(text: string, scan: Scan, ranges: readonly Range[]): Touched {
  const names = new Set<string>();
  const calls: CallSpan[] = [];
  for (const range of ranges) {
    // Each changed line, by its first non-blank character.
    let at = range.start;
    while (at < range.end) {
      const lineEnd = text.indexOf("\n", at);
      const stop = lineEnd < 0 || lineEnd >= range.end ? range.end : lineEnd;
      let first = at;
      while (first < stop && /\s/.test(text[first]!)) first += 1;
      if (first < stop) {
        const span = innermost(scan.spans, first);
        if (span) names.add(span.name);
      }
      at = stop + 1;
    }
    for (const span of scan.spans) if (span.start >= range.start && span.end <= range.end) names.add(span.name);
    for (const call of scan.calls) {
      if (call.end <= range.start || call.start >= range.end) continue;
      if (innermost(scan.spans, call.start) !== null) continue;
      if (!calls.includes(call)) calls.push(call);
    }
  }
  return { names: [...names], calls };
}
