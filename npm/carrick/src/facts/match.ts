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

/**
 * A call: `from` where its callee starts (`router` in `router.post(`), `start`
 * at its `(`, `end` after its `)`, the callee's last name (`post`), and the
 * string literals passed to it directly.
 */
export type CallSpan = { from: number; start: number; end: number; member: string | null; literals: string[] };

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
/** Words before a declaration that belong to its span. */
const DECLARATION_PREFIXES = new Set(["export", "default", "async", "declare"]);
/** Words in a type after which a `{` opens an object type, not a body. */
const TYPE_OPERATORS = new Set(["is", "asserts", "keyof", "typeof", "extends", "infer", "readonly", "unique"]);
const CLASS_MODIFIERS = new Set([
  "static", "async", "get", "set", "public", "private", "protected", "readonly", "override", "abstract", "declare", "accessor",
]);

function isIdStart(ch: string): boolean {
  return /[A-Za-z_$#]/.test(ch) || ch.charCodeAt(0) > 127;
}

function isIdPart(ch: string): boolean {
  return /[A-Za-z0-9_$]/.test(ch) || ch.charCodeAt(0) > 127;
}

/** After these a `<` in a `.tsx` or `.jsx` file opens an element. */
const JSX_AFTER_PUNC = new Set(["(", ",", "=", ":", "?", "&&", "||", "??", "=>", "{", "[", ";", "!"]);
const JSX_AFTER_WORD = new Set(["return", "yield", "await", "default"]);
/** A `<` that opens type parameters in a `.tsx` file: `<T,>` or `<T extends X>`. */
const TYPE_PARAMS_IN_JSX = /^<\s*[A-Za-z_$][\w$]*\s*(?:,|extends\s+[^=\s])/;

/**
 * The tokens of `text`, or null when a string, template, comment, regex or
 * (with `jsx`) element does not end. An element is one opaque token; the
 * code in its `{...}` containers is tokenized after it.
 */
export function tokenize(text: string, jsx = false): Token[] | null {
  try {
    return lexAll(text, jsx);
  } catch {
    return null;
  }
}

function lexAll(text: string, jsx: boolean): Token[] {
  const BAIL = new Error("unreadable");
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

  // Whether a `<` at `i` opens an element: in expression position, before a
  // tag name or `>`, and not a generic arrow's type parameters (`<T,>`,
  // `<T extends X>`), which a component file must spell that way.
  const jsxStarts = (): boolean => {
    if (!jsx || !/[A-Za-z>]/.test(text[i + 1] ?? "")) return false;
    if (TYPE_PARAMS_IN_JSX.test(text.slice(i, i + 200))) return false;
    const prev = prevSignificant();
    return (
      prev === undefined ||
      (prev.kind === "punc" && JSX_AFTER_PUNC.has(prev.value)) ||
      (prev.kind === "id" && JSX_AFTER_WORD.has(prev.value))
    );
  };

  // A tag's inside, from after `<` to after its `>`; true when it closed itself (`/>`).
  const jsxTag = (): boolean => {
    while (i < n) {
      const c = text[i]!;
      if (c === "\n") {
        line += 1;
        i += 1;
        continue;
      }
      if (c === '"' || c === "'") {
        const close = skipString(i);
        if (close === null) throw BAIL;
        i = close;
        continue;
      }
      if (c === "{") {
        i += 1;
        lex(true);
        i += 1;
        continue;
      }
      if (c === "/" && text[i + 1] === ">") {
        i += 2;
        return true;
      }
      if (c === ">") {
        i += 1;
        return false;
      }
      i += 1;
    }
    throw BAIL;
  };

  // One element from its `<` to after its closing tag.
  const jsxElement = (): void => {
    i += 1;
    if (jsxTag()) return;
    let depth = 1;
    while (i < n) {
      const c = text[i]!;
      if (c === "\n") {
        line += 1;
        i += 1;
        continue;
      }
      if (c === "{") {
        i += 1;
        lex(true);
        i += 1;
        continue;
      }
      if (c === "<") {
        if (text[i + 1] === "/") {
          const close = text.indexOf(">", i);
          if (close < 0) throw BAIL;
          i = close + 1;
          depth -= 1;
          if (depth === 0) return;
          continue;
        }
        i += 1;
        if (!jsxTag()) depth += 1;
        continue;
      }
      i += 1;
    }
    throw BAIL;
  };

  // Tokens from `i` to the end, or (with `untilBrace`) to the `}` that closes
  // the container it started in, leaving `i` on that `}`.
  function lex(untilBrace: boolean): void {
    let depth = 0;
  while (i < n) {
    const ch = text[i]!;
    if (ch === "\n") {
      line += 1;
      i += 1;
      continue;
    }
    if (ch === "}" && untilBrace && depth === 0) return;
    if (ch === "{") depth += 1;
    else if (ch === "}") depth -= 1;
    if (ch === "<" && jsxStarts()) {
      const token: Token = { kind: "tpl", value: "", start: i, end: i, line };
      tokens.push(token);
      jsxElement();
      token.end = i;
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
      if (close < 0) throw BAIL;
      for (let k = i; k < close; k += 1) if (text[k] === "\n") line += 1;
      i = close + 2;
      continue;
    }
    const startLine = line;
    if (ch === "'" || ch === '"') {
      const close = skipString(i);
      if (close === null) throw BAIL;
      tokens.push({ kind: "str", value: unquote(text.slice(i + 1, close - 1)), start: i, end: close, line: startLine });
      i = close;
      continue;
    }
    if (ch === "`") {
      const done = skipTemplate(i);
      if (done === null) throw BAIL;
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
          if (c === "\n") throw BAIL;
          if (c === "\\") {
            k += 2;
            continue;
          }
          if (c === "[") inClass = true;
          else if (c === "]") inClass = false;
          else if (c === "/" && !inClass) break;
          k += 1;
        }
        if (k >= n) throw BAIL;
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
    if (untilBrace) throw BAIL;
  }

  lex(false);
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

/** Scan `text` for named spans and calls; `jsx` for a `.tsx` or `.jsx` file. */
export function scanSource(text: string, jsx = false): Scan {
  const tokens = tokenize(text, jsx);
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
  // Set when a declaration is seen that this scanner cannot place: the file is
  // then not read at all, so an edit in it is never handed to an outer function.
  let unplaced = false;

  /** The index after the `>` closing a type parameter or argument list opened at `from`, or -1. */
  const angleEnd = (from: number, limit: number): number => {
    let depth = 0;
    for (let k = from; k < limit; k += 1) {
      const token = t(k)!;
      if (token.kind !== "punc") continue;
      if (OPEN[token.value]) {
        k = match.get(k)!;
        continue;
      }
      if (token.value === "<") depth += 1;
      else if (token.value === ">") {
        depth -= 1;
        if (depth === 0) return k + 1;
      } else if (token.value === ";" || CLOSE.has(token.value)) return -1;
    }
    return -1;
  };

  /**
   * At the `<` of a call's type arguments (`new Map<string, T[]>(`), the
   * index of the call's `(`, so their commas are not read as separators; else -1.
   */
  const typeArgsCall = (k: number, limit: number): number => {
    if (!punc(k, "<") || !is(k - 1, "id")) return -1;
    const after = angleEnd(k, limit);
    return after > 0 && punc(after, "(") ? after : -1;
  };

  /** Past a type parameter list at `k` when there is one; -1 when it does not close. */
  const pastTypeParams = (k: number, limit: number): number => (punc(k, "<") ? angleEnd(k, limit) : k);

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
    // The callee: its last name, and where its member chain starts.
    let last = punc(index - 1, "?.") ? index - 2 : index - 1;
    const member = is(last, "id") ? t(last)!.value : null;
    while (is(last, "id") && (punc(last - 1, ".") || punc(last - 1, "?.")) && is(last - 2, "id")) last -= 2;
    const from = is(last, "id") ? t(last)!.start : t(index - 1)!.start;
    calls.push({ from, start: token.start, end: tokens[close]!.end, member, literals });
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
          // After a word or bracket that ends a type the `{` is the body; after
          // one that opens a type (`v is {`, `T extends U ? {`) it is an object
          // type. After anything else the return type did not read: the file is not read.
          const before = t(k - 1)!;
          const typeOpener =
            (before.kind === "punc" && [":", "|", "&", "<", ",", "(", "=>", "?"].includes(before.value)) ||
            (before.kind === "id" && TYPE_OPERATORS.has(before.value));
          const typeEnds =
            before.kind === "id" || before.kind === "str" || before.kind === "num" || before.kind === "tpl" ||
            (before.kind === "punc" && [")", "]", "}", ">"].includes(before.value));
          if (!typeOpener && !typeEnds) {
            unplaced = true;
            return -1;
          }
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
      const call = typeArgsCall(k, limit);
      if (call > 0) {
        k = call;
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
      k = pastTypeParams(k, limit);
      if (k < 0 || !punc(k, "(")) return null;
      const body = bodyAfterParams(match.get(k)!, limit);
      if (body < 0) return null;
      return { bodyOpen: body, after: match.get(body)! + 1, exprFrom: -1 };
    }
    let arrow = -1;
    if (is(k, "id") && punc(k + 1, "=>")) arrow = k + 1;
    else if (punc(k, "(") || punc(k, "<")) {
      k = pastTypeParams(k, limit);
      if (k < 0 || !punc(k, "(")) return null;
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
    // The span starts at its modifiers: `export`, `export default`, `async`, `declare`.
    let first = declStart;
    while (first > 0 && is(first - 1, "id") && DECLARATION_PREFIXES.has(t(first - 1)!.value)) first -= 1;
    spans.push({ name, start: tokens[first]!.start, end: last.end });
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
        if (is(k, "id")) {
          const params = pastTypeParams(k + 1, limit);
          if (params < 0 || !punc(params, "(")) {
            unplaced = true;
            return;
          }
          const fn = functionAt(i, limit);
          if (fn) {
            i = named(t(k)!.value, i, fn);
            continue;
          }
          // An overload or `declare` signature: no body to place.
          i = match.get(params)! + 1;
          continue;
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
          // A function this scanner did not read (a generic it cannot close, an
          // `async` or `function` value it cannot follow): not read at all.
          if (punc(value, "<") || is(value, "id", "function") || (is(value, "id", "async") && !punc(value + 1, "."))) {
            unplaced = true;
            return;
          }
          i = value;
          continue;
        }
      }
      if (token.kind === "id" && token.value === "class" && is(i + 1, "id")) {
        const name = t(i + 1)!.value;
        let k = i + 2;
        // The heritage clause up to the body; type arguments may hold `{`.
        while (k >= 0 && k < limit && !punc(k, "{")) {
          k = punc(k, "(") || punc(k, "[") ? match.get(k)! + 1 : punc(k, "<") ? angleEnd(k, limit) : k + 1;
        }
        if (k < 0) {
          unplaced = true;
          return;
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
      const call = typeArgsCall(k, limit);
      if (call > 0) {
        k = call;
        continue;
      }
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
      if (punc(k, "<")) {
        k = angleEnd(k, limit);
        if (k < 0 || !punc(k, "(")) {
          unplaced = true;
          return;
        }
      }
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
        // A member this scanner does not know how to read.
        unplaced = true;
        return;
      }
      if (punc(k, "?") || punc(k, "!")) k += 1;
      if (punc(k, "<")) {
        k = angleEnd(k, limit);
        if (k < 0 || !punc(k, "(")) {
          unplaced = true;
          return;
        }
      }
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
  if (unplaced) return { ok: false, spans: [], calls: [] };
  return { ok: true, spans, calls };
}

/** A changed region of the file, as character offsets (end exclusive). */
export type Range = { start: number; end: number };

/**
 * Where `snippets` sit in `text`. A snippet found nowhere, or in more than one
 * place, gives no range: which copy the edit wrote is not known.
 */
export function rangesOf(text: string, snippets: readonly string[]): Range[] {
  const ranges: Range[] = [];
  for (const snippet of snippets) {
    if (snippet.trim().length === 0) continue;
    const at = text.indexOf(snippet);
    if (at < 0 || text.indexOf(snippet, at + 1) >= 0) continue;
    ranges.push({ start: at, end: at + snippet.length });
  }
  return ranges;
}

function lineStarts(text: string): number[] {
  const starts = [0];
  for (let at = text.indexOf("\n"); at >= 0; at = text.indexOf("\n", at + 1)) starts.push(at + 1);
  return starts;
}

/**
 * Where lines were removed, as offsets: a removal before the 1-based line N
 * of the file as it is now sits at the start of line N (the end of the text
 * when N is past the last line).
 */
export function gapsOfLines(text: string, removedBefore: readonly number[]): number[] {
  const starts = lineStarts(text);
  return [...new Set(removedBefore)].map((line) => starts[line - 1] ?? text.length);
}

/** The ranges of the 1-based `lines` of `text`. */
export function rangesOfLines(text: string, lines: readonly number[]): Range[] {
  const starts = lineStarts(text);
  const ranges: Range[] = [];
  for (const line of new Set(lines)) {
    const start = starts[line - 1];
    if (start === undefined) continue;
    const next = starts[line];
    ranges.push({ start, end: next === undefined ? text.length : next - 1 });
  }
  return ranges;
}

/** The largest index in `0..length` whose `key` is below `bound` (or at it, with `orEqual`), else -1. */
function lastBelow(length: number, key: (index: number) => number, bound: number, orEqual: boolean): number {
  let low = 0;
  let high = length - 1;
  let found = -1;
  while (low <= high) {
    const mid = (low + high) >> 1;
    const value = key(mid);
    if (value < bound || (orEqual && value === bound)) {
      found = mid;
      low = mid + 1;
    } else high = mid - 1;
  }
  return found;
}

/**
 * The named spans in start order with each one's parent (they nest: the
 * walk records a thing before its body), so the innermost span holding an
 * offset is found by one binary search and a walk up, not a pass over all.
 */
function spanIndex(spans: readonly NamedSpan[]) {
  const sorted = [...spans].sort((a, b) => a.start - b.start || b.end - a.end);
  const parent: number[] = [];
  const stack: number[] = [];
  for (const [index, span] of sorted.entries()) {
    while (stack.length > 0 && sorted[stack[stack.length - 1]!]!.end <= span.start) stack.pop();
    parent.push(stack.length > 0 ? stack[stack.length - 1]! : -1);
    stack.push(index);
  }
  return {
    /** The innermost span holding `offset` (`start < offset` with `strict`), or null. */
    at(offset: number, strict: boolean): NamedSpan | null {
      let index = lastBelow(sorted.length, (k) => sorted[k]!.start, offset, !strict);
      while (index >= 0 && sorted[index]!.end <= offset) index = parent[index]!;
      return index >= 0 ? sorted[index]! : null;
    },
    /** Every span wholly inside `range`. */
    within(range: Range): NamedSpan[] {
      const out: NamedSpan[] = [];
      for (let k = lastBelow(sorted.length, (j) => sorted[j]!.start, range.start, false) + 1; k < sorted.length; k += 1) {
        const span = sorted[k]!;
        if (span.start >= range.end) break;
        if (span.end <= range.end) out.push(span);
      }
      return out;
    },
  };
}

/** What an edit touched: the named things, and the unnamed calls it reached. */
export type Touched = { names: string[]; calls: CallSpan[] };

/**
 * The names an edit over `ranges` touched (the innermost named encloser of
 * each changed line, and every named thing inside a changed range), and the
 * calls outside every named thing whose text (from the callee to the
 * closing `)`) covers part of a changed line. A removal at offset `gap`
 * (`gapsOfLines`) touches only the innermost thing, or top-level call, it
 * sits strictly inside: never one whose first line it is just above, and
 * never one it follows.
 */
export function touched(text: string, scan: Scan, ranges: readonly Range[], gaps: readonly number[] = []): Touched {
  const names = new Set<string>();
  const calls = new Set<CallSpan>();
  const index = spanIndex(scan.spans);
  // Removals and changed lines that no named thing holds, in offset order.
  const topGaps: number[] = [];
  for (const gap of gaps) {
    const span = index.at(gap, true);
    if (span) names.add(span.name);
    else topGaps.push(gap);
  }
  const topLines: Array<{ first: number; stop: number }> = [];
  for (const range of ranges) {
    // Each changed line, by its first non-blank character.
    let at = range.start;
    while (at < range.end) {
      const lineEnd = text.indexOf("\n", at);
      const stop = lineEnd < 0 || lineEnd >= range.end ? range.end : lineEnd;
      let first = at;
      while (first < stop && /\s/.test(text[first]!)) first += 1;
      if (first < stop) {
        const span = index.at(first, false);
        if (span) names.add(span.name);
        else topLines.push({ first, stop });
      }
      at = stop + 1;
    }
    for (const span of index.within(range)) names.add(span.name);
  }
  if (topGaps.length === 0 && topLines.length === 0) return { names: [...names], calls: [] };
  topGaps.sort((a, b) => a - b);
  topLines.sort((a, b) => a.first - b.first);
  for (const call of scan.calls) {
    if (index.at(call.start, false) !== null) continue;
    // The first removal after the callee starts, and the first changed line ending after it.
    const gap = topGaps[lastBelow(topGaps.length, (k) => topGaps[k]!, call.from, true) + 1];
    let hit = gap !== undefined && gap < call.end;
    for (let k = lastBelow(topLines.length, (j) => topLines[j]!.stop, call.from, true) + 1; !hit && k < topLines.length; k += 1) {
      if (topLines[k]!.first >= call.end) break;
      hit = true;
    }
    if (hit) calls.add(call);
  }
  return { names: [...names], calls: [...calls] };
}
