/**
 * Global / module-augmentation detection (design doc, Capture step 4).
 *
 * An entry-rooted program drops `declare global` and `declare module "x"`
 * augmentation files outside the entry's import graph, and because emit
 * proceeds despite errors the tree would ship with dangling global
 * references. Capture therefore scans the tsconfig's full file list and adds
 * every augmentation-declaring file as an extra emit root.
 *
 * This deliberately over-approximates "reachable from the closure's symbols":
 * including an unrelated augmentation costs tree bytes, excluding a needed
 * one silently corrupts the closure. Cheap syntactic prefilter first, real
 * parse only on candidates.
 */

import ts from 'typescript';
import * as fs from 'node:fs';

const PREFILTER = /declare\s+(?:global|module)\b/;

export function declaresAugmentation(sourceFile: ts.SourceFile): boolean {
  // Augmentations only have their augmentation semantics inside a module;
  // in a script file `declare module "x"` is an ambient module declaration,
  // which the closure treatment still wants shipped when present.
  for (const stmt of sourceFile.statements) {
    if (!ts.isModuleDeclaration(stmt)) continue;
    if (stmt.name.kind === ts.SyntaxKind.Identifier) {
      // `declare global` parses as a ModuleDeclaration with the
      // GlobalAugmentation flag and an Identifier name.
      if ((stmt.flags & ts.NodeFlags.GlobalAugmentation) !== 0) return true;
    } else if (ts.isStringLiteral(stmt.name)) {
      return true;
    }
  }
  return false;
}

/**
 * Absolute paths of files in `fileNames` that declare global or module
 * augmentations. Files that fail to read/parse are skipped (they will fail
 * loudly elsewhere if they matter).
 */
export function findAugmentationFiles(fileNames: readonly string[]): string[] {
  const found: string[] = [];
  for (const fileName of fileNames) {
    let text: string;
    try {
      text = fs.readFileSync(fileName, 'utf8');
    } catch {
      continue;
    }
    if (!PREFILTER.test(text)) continue;
    const sourceFile = ts.createSourceFile(
      fileName,
      text,
      ts.ScriptTarget.Latest,
      /* setParentNodes */ false
    );
    if (declaresAugmentation(sourceFile)) found.push(fileName);
  }
  return found;
}
