/**
 * Module-specifier helpers shared across the capture bundle: extraction from
 * declaration text, external/internal classification, and tsconfig-`paths`
 * pattern matching for the post-emit rewrite pass.
 */

export function isRelative(spec: string): boolean {
  return spec.startsWith('./') || spec.startsWith('../') || spec.startsWith('/');
}

/** zod -> zod, @scope/pkg/sub -> @scope/pkg, pkg/sub -> pkg */
export function packageNameOf(spec: string): string {
  const parts = spec.split('/');
  return spec.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0];
}

/** Extract every module specifier mentioned in a .d.ts text: `from "x"`,
 * `import "x"`, and `import("x")` type references. */
export function collectSpecifiers(text: string): Set<string> {
  const specs = new Set<string>();
  const re = /(?:from\s+|import\s+|import\s*\(\s*)["']([^"']+)["']/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    specs.add(m[1]);
  }
  return specs;
}

/**
 * One parsed tsconfig `paths` mapping: pattern "prefix*suffix" (or exact) to
 * substitution targets resolved against the config's path base.
 */
export interface PathsPattern {
  /** Original pattern, e.g. "@app/*" */
  pattern: string;
  prefix: string;
  /** undefined for exact (starless) patterns */
  suffix?: string;
  /** Absolute target templates ("*" preserved), in declaration order. */
  targets: string[];
}

/**
 * Match a specifier against a pattern. Returns the "*" capture ('' for exact
 * matches) or undefined when there is no match.
 */
export function matchPathsPattern(spec: string, p: PathsPattern): string | undefined {
  if (p.suffix === undefined) {
    return spec === p.pattern ? '' : undefined;
  }
  if (
    spec.length >= p.prefix.length + p.suffix.length &&
    spec.startsWith(p.prefix) &&
    spec.endsWith(p.suffix)
  ) {
    return spec.slice(p.prefix.length, spec.length - p.suffix.length);
  }
  return undefined;
}

/**
 * Rewrite every specifier occurrence in a .d.ts text through `map`. The map
 * receives each specifier and returns a replacement or undefined (keep).
 * Operates on the same syntactic positions collectSpecifiers finds, so the
 * two stay consistent by construction.
 */
export function rewriteSpecifiers(
  text: string,
  map: (spec: string) => string | undefined
): { text: string; rewrites: number } {
  let rewrites = 0;
  const out = text.replace(
    /((?:from\s+|import\s+|import\s*\(\s*)["'])([^"']+)(["'])/g,
    (whole, open: string, spec: string, close: string) => {
      const replacement = map(spec);
      if (replacement === undefined || replacement === spec) return whole;
      rewrites++;
      return `${open}${replacement}${close}`;
    }
  );
  return { text: out, rewrites };
}
