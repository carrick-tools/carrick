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
 *
 * One more reading rides the same walk (carrick#1538): the sent side's
 * unwidened alias, where a probe carries it. A member-level `any` there would
 * let that reading assign to anything, so the classifier lets it clear a
 * mismatch only when this walk ran over it and found nothing.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { sidecarCompilerOptions } from './compiler-options.js';
import type { TypeProvenance } from './api.js';
import { findDisqualifyingTopTypes, findForeignDeclarationPositions, provenanceOf } from './deep-walk.js';
import { UNWIDENED_LOCAL_NAME, type ProbePlan, type Side } from './check-probe.js';
import type { AssembledWorkspace } from './check-workspace.js';
import { isGlobalScopeFile } from './self-check.js';

/** Deep findings for one pair, per probe side. */
export interface PairDeepFindings {
  sent: TypeProvenance[];
  expected: TypeProvenance[];
  /**
   * Findings on the sent side's unwidened reading (carrick#1538). Absent when
   * the probe carries none, or when its export could not be resolved.
   */
  sentUnwidened?: TypeProvenance[];
}

/** The one compiler program over the assembled probes, shared by every
 * post-judge walk (fact-ness, and the field-level report the mismatch text
 * names). Built once: two `createProgram` calls over the same file set would
 * double the check phase's most expensive step and could not disagree usefully
 * anyway. `undefined` when it cannot be built at all. */
export interface ProbeProgram {
  program: ts.Program;
  checker: ts.TypeChecker;
  probesDir: string;
}

export function openProbeProgram(
  probesDir: string,
  plans: ProbePlan[]
): ProbeProgram | undefined {
  if (plans.length === 0) return undefined;
  const configPath = path.join(probesDir, 'tsconfig.json');
  if (!fs.existsSync(configPath)) return undefined;
  try {
    const raw = ts.readConfigFile(configPath, (f) => fs.readFileSync(f, 'utf8'));
    if (raw.error) return undefined;
    const parsed = ts.parseJsonConfigFileContent(raw.config, ts.sys, probesDir);
    const fileNames = plans
      .map((plan) => path.join(probesDir, 'probes', plan.fileName))
      .filter((f) => fs.existsSync(f));
    if (fileNames.length === 0) return undefined;
    const program = ts.createProgram(fileNames, sidecarCompilerOptions({ ...parsed.options, noEmit: true }));
    return { program, checker: program.getTypeChecker(), probesDir };
  } catch {
    return undefined;
  }
}

/**
 * Walk both sides of every probe in the assembled workspace.
 *
 * Returns an empty map when the program could not be built — absence of
 * findings must never be read as "clean", so the caller treats a missing entry
 * as unresolved rather than resolved.
 */
export function probeDeepFindings(
  opened: ProbeProgram | undefined,
  plans: ProbePlan[]
): Map<string, PairDeepFindings> {
  const results = new Map<string, PairDeepFindings>();
  if (!opened) return results;
  const { program, checker, probesDir } = opened;
  for (const plan of plans) {
    const file = program.getSourceFile(path.join(probesDir, 'probes', plan.fileName));
    if (!file) continue;
    const sent = walkImportedAlias(file, 'Sent', program, checker);
    const expected = walkImportedAlias(file, 'Expected', program, checker);
    if (sent === undefined || expected === undefined) continue;
    const sentUnwidened =
      plan.unwidenedFromLine === undefined
        ? undefined
        : walkImportedAlias(file, UNWIDENED_LOCAL_NAME, program, checker);
    results.set(plan.pairId, {
      sent,
      expected,
      ...(sentUnwidened === undefined ? {} : { sentUnwidened }),
    });
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
  localName: ProbeLocalName,
  program: ts.Program,
  checker: ts.TypeChecker
): TypeProvenance[] | undefined {
  const imported = importedAliasType(file, localName, checker);
  if (!imported) return undefined;
  return findDisqualifyingTopTypes(imported.type, program, checker, imported.at).map((finding) =>
    provenanceOf(finding)
  );
}

type ProbeLocalName = 'Sent' | 'Expected' | typeof UNWIDENED_LOCAL_NAME;

/** The declared type of one of the probe's imported surface aliases, and the
 * import name it is read at; `undefined` when it does not resolve. */
function importedAliasType(
  file: ts.SourceFile,
  localName: ProbeLocalName,
  checker: ts.TypeChecker
): { type: ts.Type; at: ts.Node } | undefined {
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
      return { type, at: element.name };
    }
  }
  return undefined;
}

/**
 * What one side of a pair reads of another service's declarations
 * (carrick#1047 J1-1): the first position and the service that declares it,
 * where the walk ran out of budget when the walk ran out of budget before it could rule one out,
 * and `unwalked` when no program could be built to walk it in.
 */
export type SideForeign = { service: string; path: string } | { exhaustedAt: string } | 'unwalked';

/** Per side of a pair; a side absent here reads only its own service. */
export type PairForeignFindings = Partial<Record<Side, SideForeign>>;

/**
 * Which probed sides read a declaration that only another service declares
 * (carrick#1047 J1-1).
 *
 * Each service compiles alone. The judge loads every probed stub into one
 * program, so a global-scope file of one stub (a script, a `declare global`, a
 * module augmentation) can merge into a name another stub's type reads, and
 * that type is then one neither service compiles. A side can only read
 * another stub's declaration through such a file: a stub that names a sibling
 * by its package name does not resolve in this workspace (carrick#2970). So a
 * side is walked only when the program holds a global-scope file of another
 * stub, and otherwise costs nothing.
 *
 * A declaration in another stub's tree is not foreign when its text equals a
 * declaration of the same symbol in the side's own tree: a typing file two
 * services both carry says the same thing in each.
 *
 * Fails closed. A walk out of budget is `exhaustedAt`; a run with no program
 * whose workspace holds another stub's global-scope file is `unwalked`.
 */
export function pairForeignFindings(
  opened: ProbeProgram | undefined,
  plans: ProbePlan[],
  ws: AssembledWorkspace
): Map<string, PairForeignFindings> {
  const results = new Map<string, PairForeignFindings>();
  const dirOfService = new Map(ws.stubs.map((s) => [s.serviceName, s.packageDir]));
  const roots = [...new Set([ws.workspaceDir, fs.realpathSync(ws.workspaceDir)])].map((r) =>
    path.join(r, 'packages').split(path.sep).join('/') + '/'
  );
  const stubDirs = new Set(ws.stubs.map((s) => s.packageDir));
  /** The stub package dir a file lies in, outside its installed packages. */
  const stubDirOf = (fileName: string): string | undefined => {
    const normal = fileName.split(path.sep).join('/');
    for (const root of roots) {
      if (!normal.startsWith(root)) continue;
      const rest = normal.slice(root.length);
      const dir = rest.slice(0, rest.indexOf('/'));
      if (rest.includes('/node_modules/') || !stubDirs.has(dir)) return undefined;
      return dir;
    }
    return undefined;
  };

  const globalDirs = opened
    ? new Set(
        opened.program
          .getSourceFiles()
          .filter((sf) => stubDirOf(sf.fileName) !== undefined && isGlobalScopeFile(sf))
          .map((sf) => stubDirOf(sf.fileName)!)
      )
    : globalScopeDirsOnDisk(ws);
  const reachable = (dir: string | undefined): boolean =>
    [...globalDirs].some((g) => g !== dir);
  if (![...dirOfService.values()].some(reachable)) return results;

  const sideOf = (plan: ProbePlan, side: Side) => plan.spec[side].service_name;
  if (!opened) {
    for (const plan of plans) {
      const found: PairForeignFindings = {};
      for (const side of ['producer', 'consumer'] as const) {
        if (reachable(dirOfService.get(sideOf(plan, side)))) found[side] = 'unwalked';
      }
      if (Object.keys(found).length > 0) results.set(plan.pairId, found);
    }
    return results;
  }

  const { program, checker, probesDir } = opened;
  const normalText = (d: ts.Declaration): string => d.getText().replace(/\s+/g, ' ');
  for (const plan of plans) {
    const file = program.getSourceFile(path.join(probesDir, 'probes', plan.fileName));
    if (!file) continue;
    const found: PairForeignFindings = {};
    for (const side of ['producer', 'consumer'] as const) {
      const own = dirOfService.get(sideOf(plan, side));
      if (!reachable(own)) continue;
      const foreignOf = (symbol: ts.Symbol): string | undefined => {
        const declarations = symbol.declarations ?? [];
        const ownTexts = new Set(
          declarations.filter((d) => stubDirOf(d.getSourceFile().fileName) === own).map(normalText)
        );
        for (const d of declarations) {
          const dir = stubDirOf(d.getSourceFile().fileName);
          if (dir === undefined || dir === own || ownTexts.has(normalText(d))) continue;
          return ws.serviceOfPackageDir(dir) ?? dir;
        }
        return undefined;
      };
      // The sent side is read twice where the probe carries its unwidened
      // reading (carrick#1538), and either reading may clear the pair.
      const names: ProbeLocalName[] =
        side === plan.direction.sent
          ? plan.unwidenedFromLine === undefined
            ? ['Sent']
            : ['Sent', UNWIDENED_LOCAL_NAME]
          : ['Expected'];
      for (const name of names) {
        const imported = importedAliasType(file, name, checker);
        if (!imported) continue;
        const walk = findForeignDeclarationPositions(imported.type, program, checker, imported.at, foreignOf);
        if (walk.hit) {
          found[side] = walk.hit;
          break;
        }
        if (walk.exhaustedAt !== undefined) found[side] = { exhaustedAt: walk.exhaustedAt };
      }
    }
    if (Object.keys(found).length > 0) results.set(plan.pairId, found);
  }
  return results;
}

/** The stub dirs whose emitted tree holds a global-scope file, read from
 * disk: the answer when no program could be built. */
function globalScopeDirsOnDisk(ws: AssembledWorkspace): Set<string> {
  const dirs = new Set<string>();
  for (const stub of ws.stubs) {
    const typesDir = path.join(ws.workspaceDir, 'packages', stub.packageDir, 'types');
    const files = fs.existsSync(typesDir)
      ? (fs.readdirSync(typesDir, { recursive: true }) as string[]).filter((f) => f.endsWith('.ts'))
      : [];
    for (const rel of files) {
      const abs = path.join(typesDir, rel);
      const source = ts.createSourceFile(abs, fs.readFileSync(abs, 'utf8'), ts.ScriptTarget.Latest);
      if (isGlobalScopeFile(source)) {
        dirs.add(stub.packageDir);
        break;
      }
    }
  }
  return dirs;
}
