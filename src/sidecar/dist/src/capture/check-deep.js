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
import { findDisqualifyingTopTypes, provenanceOf } from './deep-walk.js';
import { UNWIDENED_LOCAL_NAME } from './check-probe.js';
export function openProbeProgram(probesDir, plans) {
    if (plans.length === 0)
        return undefined;
    const configPath = path.join(probesDir, 'tsconfig.json');
    if (!fs.existsSync(configPath))
        return undefined;
    try {
        const raw = ts.readConfigFile(configPath, (f) => fs.readFileSync(f, 'utf8'));
        if (raw.error)
            return undefined;
        const parsed = ts.parseJsonConfigFileContent(raw.config, ts.sys, probesDir);
        const fileNames = plans
            .map((plan) => path.join(probesDir, 'probes', plan.fileName))
            .filter((f) => fs.existsSync(f));
        if (fileNames.length === 0)
            return undefined;
        const program = ts.createProgram(fileNames, sidecarCompilerOptions({ ...parsed.options, noEmit: true }));
        return { program, checker: program.getTypeChecker(), probesDir };
    }
    catch {
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
export function probeDeepFindings(opened, plans) {
    const results = new Map();
    if (!opened)
        return results;
    const { program, checker, probesDir } = opened;
    for (const plan of plans) {
        const file = program.getSourceFile(path.join(probesDir, 'probes', plan.fileName));
        if (!file)
            continue;
        const sent = walkImportedAlias(file, 'Sent', program, checker);
        const expected = walkImportedAlias(file, 'Expected', program, checker);
        if (sent === undefined || expected === undefined)
            continue;
        const sentUnwidened = plan.unwidenedFromLine === undefined
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
function walkImportedAlias(file, localName, program, checker) {
    for (const statement of file.statements) {
        if (!ts.isImportDeclaration(statement))
            continue;
        const bindings = statement.importClause?.namedBindings;
        if (!bindings || !ts.isNamedImports(bindings))
            continue;
        for (const element of bindings.elements) {
            if (element.name.text !== localName)
                continue;
            const local = checker.getSymbolAtLocation(element.name);
            if (!local)
                return undefined;
            const target = local.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(local) : local;
            const type = checker.getDeclaredTypeOfSymbol(target);
            if (!type)
                return undefined;
            return findDisqualifyingTopTypes(type, program, checker, element.name).map((finding) => provenanceOf(finding));
        }
    }
    return undefined;
}
