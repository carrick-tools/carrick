/**
 * DefinitionResolver - Resolves surface type aliases from a v2 capture stub
 * package's declaration tree using the TypeScript compiler.
 *
 * Two forms are produced per alias:
 *  - `definition`: the declaration *as written* (named refs preserved). For a
 *    surface alias `export type A = import('./m').Order;` this follows the
 *    alias to its target declaration in the stub tree (`interface Order {...}`)
 *    so the definition keeps its real name and members; when the target is
 *    anonymous (inline object types, node-builder prints) the surface alias
 *    line itself is the as-written form.
 *  - `expanded`: the fully *structural* form, with every named member type
 *    inlined to its member structure, recursively.
 *
 * `type.getText(node, NoTruncation)` does NOT inline named members — the
 * compiler prints a referenced type by its symbol name when that symbol is in
 * scope (`total: Money`, not `total: { amountCents: number; currency: string }`).
 * The structural form is produced by `expandTypeStructural` (shared with the
 * inference path in `type-inferrer.ts`), which walks the resolved `Type` and
 * rebuilds the inlined text.
 *
 * Each resolve call builds its own throwaway in-memory project over the stub
 * tree, so the warm sidecar's long-lived project never sees stub files and
 * cannot accumulate stale trees across requests.
 */

import * as path from 'node:path';
import * as fs from 'node:fs';
import { Project, Node, type SourceFile } from 'ts-morph';
import {
  expandTypeStructural,
  type ExpandOrigin,
} from './type-structural-expander.js';

export interface ResolvedDefinition {
  type_alias: string;
  /** Original declaration text as written (preserves named types) */
  definition: string;
  /** Fully structural form: named member types inlined to their structure */
  expanded: string;
}

export class DefinitionResolver {
  private readonly project: Project;

  constructor(options: { project: Project }) {
    this.project = options.project;
  }

  /**
   * Resolve surface aliases from a capture stub package directory
   * (`<stub_dir>/types/surface.d.ts` + its declaration tree).
   *
   * Uses a DEDICATED project with `moduleResolution: Bundler`, not the
   * repo's own project: the stub tree's relative import-types are
   * extensionless, which a NodeNext-configured repo project silently fails
   * to resolve (the alias then reads as `any`). Bundler resolution accepts
   * both extensionless and `.js`-suffixed specifiers — the same policy the
   * check-phase workspace uses.
   */
  resolveFromStub(stubDir: string, aliases: string[]): ResolvedDefinition[] {
    const typesDir = path.join(stubDir, 'types');
    const surfacePath = path.join(typesDir, 'surface.d.ts');
    if (!fs.existsSync(surfacePath)) {
      this.log(`No surface.d.ts under ${stubDir}; nothing to resolve`);
      return [];
    }

    try {
      const stubProject = new Project({
        compilerOptions: {
          target: 99, // ESNext
          module: 99, // ESNext
          moduleResolution: 100, // Bundler
          strict: true,
          skipLibCheck: true,
        },
        skipAddingFilesFromTsConfig: true,
      });
      for (const filePath of walkDtsFiles(typesDir)) {
        stubProject.addSourceFileAtPath(filePath);
      }
      const surface = stubProject.getSourceFile(surfacePath);
      if (!surface) {
        this.logError(`Failed to load ${surfacePath}`);
        return [];
      }

      const results: ResolvedDefinition[] = [];
      for (const alias of aliases) {
        const result = this.resolveAlias(surface, alias, {
          program: stubProject.getProgram().compilerObject,
          repoRoot: stubDir,
        });
        if (result) {
          results.push(result);
        } else {
          this.log(`Could not resolve alias: ${alias}`);
        }
      }
      return results;
    } catch (err) {
      this.logError(
        `Resolution failed: ${err instanceof Error ? err.message : String(err)}`,
      );
      return [];
    }
  }

  /**
   * Resolve a single alias: the original text and the structural form.
   */
  private resolveAlias(
    sourceFile: SourceFile,
    alias: string,
    origin: ExpandOrigin,
  ): ResolvedDefinition | null {
    const decl =
      sourceFile.getTypeAlias(alias) ??
      sourceFile.getInterface(alias) ??
      sourceFile.getClass(alias) ??
      sourceFile.getEnum(alias);

    if (!decl) return null;

    try {
      const type = decl.getType();

      // As-written form: prefer the alias target's own declaration (the real
      // `interface Order {...}` in the tree) over the surface's import-type
      // line, so named shapes read naturally. Fall back to the alias line for
      // anonymous targets, self-referential alias symbols, or lib/external
      // declarations outside the stub tree.
      let definition = decl.getText();
      for (const symbol of [type.getAliasSymbol(), type.getSymbol()]) {
        const targetDecl = symbol?.getDeclarations()?.[0];
        if (
          targetDecl &&
          targetDecl !== decl &&
          (Node.isInterfaceDeclaration(targetDecl) ||
            Node.isTypeAliasDeclaration(targetDecl) ||
            Node.isClassDeclaration(targetDecl) ||
            Node.isEnumDeclaration(targetDecl)) &&
          !targetDecl.getSourceFile().getFilePath().includes('node_modules')
        ) {
          definition = targetDecl.getText();
          break;
        }
      }

      // Structural form — every named member inlined to its shape.
      const expanded = expandTypeStructural(type, origin);

      return { type_alias: alias, definition, expanded };
    } catch (err) {
      this.logError(
        `Failed to resolve ${alias}: ${err instanceof Error ? err.message : String(err)}`,
      );
      return null;
    }
  }

  private log(message: string): void {
    console.error(`[sidecar:definition-resolver] ${message}`);
  }

  private logError(message: string): void {
    console.error(`[sidecar:definition-resolver:error] ${message}`);
  }
}

/** All .d.ts files under a directory, depth-first, deterministic order. */
function walkDtsFiles(dir: string): string[] {
  const out: string[] = [];
  const walk = (current: string) => {
    const entries = fs
      .readdirSync(current, { withFileTypes: true })
      .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    for (const entry of entries) {
      const p = path.join(current, entry.name);
      if (entry.isDirectory()) walk(p);
      else if (entry.name.endsWith('.d.ts')) out.push(p);
    }
  };
  walk(dir);
  return out;
}
