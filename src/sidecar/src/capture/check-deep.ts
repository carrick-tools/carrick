/**
 * Check-time fact-ness walk (carrick#707, R1d).
 *
 * The probe gates prove the two sides are not WHOLLY `any`/`unknown`/`never`.
 * They cannot see a member three levels down, and a member-level `any` accepts
 * every counterparty shape — so "no diagnostic" there is not a compatibility
 * result, it is the absence of one. The capture-time walk catches most of it,
 * but only what the capture could resolve: a member that decayed through a
 * pinned external is TypeScript's `error` placeholder at capture time and is
 * deliberately excluded there, because it heals once the check installs the
 * pin (carrick#450). This walk runs after that install, over the assembled
 * probe workspace, so it sees what the capture could not.
 *
 * It is used for exactly one thing: setting `CheckVerdict.resolved`. No bucket
 * changes because of it, so a scan's verdicts are identical with and without
 * it — what changes is whether a reader is told the verdict is a fact.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import type { TypeProvenance } from './api.js';
import { findDisqualifyingTopTypes, provenanceOf } from './deep-walk.js';
import type { ProbePlan } from './check-probe.js';

/** Deep findings for one pair, per probe side. */
export interface PairDeepFindings {
  sent: TypeProvenance[];
  expected: TypeProvenance[];
}

/**
 * Walk both sides of every probe in the assembled workspace.
 *
 * Returns an empty map when the program cannot be built — absence of findings
 * must never be read as "clean", so the caller treats a missing entry as
 * unresolved rather than resolved.
 */
export function probeDeepFindings(
  probesDir: string,
  plans: ProbePlan[]
): Map<string, PairDeepFindings> {
  const results = new Map<string, PairDeepFindings>();
  if (plans.length === 0) return results;

  const configPath = path.join(probesDir, 'tsconfig.json');
  if (!fs.existsSync(configPath)) return results;

  let program: ts.Program;
  try {
    const raw = ts.readConfigFile(configPath, (f) => fs.readFileSync(f, 'utf8'));
    if (raw.error) return results;
    const parsed = ts.parseJsonConfigFileContent(raw.config, ts.sys, probesDir);
    const fileNames = plans
      .map((plan) => path.join(probesDir, 'probes', plan.fileName))
      .filter((f) => fs.existsSync(f));
    if (fileNames.length === 0) return results;
    program = ts.createProgram(fileNames, { ...parsed.options, noEmit: true });
  } catch {
    return results;
  }

  const checker = program.getTypeChecker();
  for (const plan of plans) {
    const file = program.getSourceFile(path.join(probesDir, 'probes', plan.fileName));
    if (!file) continue;
    const sent = walkImportedAlias(file, 'Sent', program, checker);
    const expected = walkImportedAlias(file, 'Expected', program, checker);
    if (sent === undefined || expected === undefined) continue;
    results.set(plan.pairId, { sent, expected });
  }
  return results;
}

/**
 * Findings for one of the probe's two imported surface aliases, or `undefined`
 * when the alias cannot be resolved at all (the import-error path, which the
 * classifier already reports; there is nothing to add here).
 */
function walkImportedAlias(
  file: ts.SourceFile,
  localName: 'Sent' | 'Expected',
  program: ts.Program,
  checker: ts.TypeChecker
): TypeProvenance[] | undefined {
  for (const statement of file.statements) {
    if (!ts.isImportDeclaration(statement)) continue;
    const bindings = statement.importClause?.namedBindings;
    if (!bindings || !ts.isNamedImports(bindings)) continue;
    for (const element of bindings.elements) {
      if (element.name.text !== localName) continue;
      const local = checker.getSymbolAtLocation(element.name);
      if (!local) return undefined;
      const target =
        local.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(local) : local;
      const type = checker.getDeclaredTypeOfSymbol(target);
      if (!type) return undefined;
      return findDisqualifyingTopTypes(type, program, checker, element.name).map(
        provenanceOf
      );
    }
  }
  return undefined;
}
