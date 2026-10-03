/**
 * Diagnostic scrub for the v2 check phase.
 *
 * Raw tsc diagnostics leak two run-varying / internal things into the most
 * interesting error class (cross-stub conflicts): stub-absolute
 * `import("/tmp/xxxx/packages/orders/types/surface")` paths inside elaboration
 * text, and the probe's `Sent`/`Expected` import aliases in the headline. This
 * pass maps every workspace-absolute path to a stable `@carrick/<service>` (or
 * `pkg@version`) label and rewrites the aliases to the real surface names, so
 * the message is user-facing AND byte-stable across runs with different temp
 * dirs. Real TS text (e.g. TS2741 "... missing in type 'StockLevel'") is
 * preserved intact.
 *
 * Seam: node builtins only; no imports needed.
 */

export interface ScrubContext {
  /** Absolute path of the scratch workspace root (fully removed from output). */
  workspaceRoot: string;
  /** The same root with symlinks resolved, as a tool that prints real paths
   * names it (pnpm, under a macOS temp dir). Absent when there is no tree. */
  workspaceRealRoot?: string;
  /** Workspace package dir (== sanitized service) -> the @carrick/<dir> label. */
  packageLabelOf: (packageDir: string) => string | undefined;
}

const IMPORT_PATH_RE = /import\("([^"]+)"\)/g;
const PNPM_SEGMENT_RE = /\/\.pnpm\/([^/]+)\/node_modules\//;
const NODE_MODULES_TAIL_RE = /\/node_modules\/((?:@[^/]+\/)?[^/]+)(?:\/|$)/;
const PACKAGES_SEGMENT_RE = /\/packages\/([^/]+)\//;

/** Map one absolute import specifier to a stable, path-free label. */
function labelForImportPath(absPath: string, ctx: ScrubContext): string {
  const normalized = absPath.split('\\').join('/');

  // pnpm isolated store: .../.pnpm/<name>@<ver>/node_modules/<name>/...
  const pnpm = normalized.match(PNPM_SEGMENT_RE);
  if (pnpm) return pnpm[1];

  // A stub tree file: .../packages/<dir>/types/surface -> @carrick/<service>
  const pkg = normalized.match(PACKAGES_SEGMENT_RE);
  if (pkg) {
    const label = ctx.packageLabelOf(pkg[1]);
    if (label) return label;
  }

  // Any other node_modules path (defensive; isolated linker should not hit).
  const nm = normalized.match(NODE_MODULES_TAIL_RE);
  if (nm) return nm[1];

  // Fall back to just stripping the workspace root prefix.
  if (normalized.startsWith(ctx.workspaceRoot.split('\\').join('/'))) {
    return normalized.slice(ctx.workspaceRoot.length).replace(/^\/+/, '');
  }
  return normalized;
}

/** Replace stub-absolute import paths and any lingering workspace root. */
export function scrubPaths(text: string, ctx: ScrubContext): string {
  let out = text.replace(IMPORT_PATH_RE, (_m, p1: string) => {
    return `import("${labelForImportPath(p1, ctx)}")`;
  });
  // Defensive: no raw temp path may survive anywhere in the message. The real
  // root goes first: the given root can be its suffix (`/private/var/...`
  // against `/var/...`), and stripping that first would strand the prefix.
  for (const root of [ctx.workspaceRealRoot, ctx.workspaceRoot]) {
    if (!root) continue;
    out = out.split(root).join('');
    out = out.split(root.split('/').join('\\')).join('');
  }
  return out;
}

/**
 * Rewrite the probe's `Sent`/`Expected` import aliases (which the compiler
 * prints in the headline) to the real surface alias names. Only quoted forms
 * are touched, so real type names in nested elaboration lines are preserved.
 */
export function rewriteAliases(
  text: string,
  sentAlias: string,
  expectedAlias: string
): string {
  return text
    .split(`'Sent'`)
    .join(`'${sentAlias}'`)
    .split(`'Expected'`)
    .join(`'${expectedAlias}'`);
}

/** Full scrub applied to an incompatible pair's diagnostic message. */
export function scrubDiagnostic(
  text: string,
  ctx: ScrubContext,
  sentAlias: string,
  expectedAlias: string
): string {
  return rewriteAliases(scrubPaths(text, ctx), sentAlias, expectedAlias);
}
