/**
 * How the init'd project resolves an import: in the mode the compiler picks
 * for the file that imports it (carrick#1619).
 *
 * Under `module` `node16`..`nodenext` the compiler reads each file's own
 * format (its nearest `package.json` `"type"`, or its extension) to decide
 * whether an import resolves with the `import` or the `require` export
 * conditions. The program asks its host to create each file with that format,
 * but ts-morph's host creates every file with the script target only, so no
 * file had a format and every import resolved as a `require`. A package whose
 * `exports` entry only has an `import` branch did not resolve at all, while
 * the capture, which uses the compiler directly, resolved the same import.
 *
 * This resolution host gives the importing file its format before its imports
 * are resolved, then resolves each import as the compiler's default does:
 * `ts.resolveModuleName` in the mode `ts.getModeForUsageLocation` reads off
 * that import. The format is only set under `node16`..`nodenext`. Under other
 * `module` settings the compiler still takes an import's mode from its syntax
 * (`import`, `require`, a `resolution-mode` attribute) and reads a file's
 * format only in narrow cases; setting it there made answers worse, because
 * ts-morph's TypeScript copy shares one resolution-cache entry across modes
 * under Bundler (carrick#1632), so those settings keep their old modes.
 *
 * The hook receives one entry per import, by module name, so one name
 * imported twice in different modes (a `require` beside an `import`) needs
 * each entry matched to its own import. It is, by position, when every import
 * of the name is being resolved; when only some of them are (the compiler
 * reusing earlier answers) and their modes differ, the entry cannot be placed
 * and is left unresolved rather than given another import's mode.
 */

import { ts, type ResolutionHostFactory } from 'ts-morph';

/** Whether the compiler decides an import's mode from the importing file's format. */
function formatDecidesMode(options: ts.CompilerOptions): boolean {
  const kind = options.module;
  return kind !== undefined && kind >= ts.ModuleKind.Node16 && kind <= ts.ModuleKind.NodeNext;
}

/**
 * The literals in `file` the program resolves, by module name and in source
 * order: its imports, then its module augmentations. Neither list is in the
 * public declaration file.
 */
function usagesOf(file: ts.SourceFile): Map<string, ts.StringLiteralLike[]> {
  const internal = file as ts.SourceFile & {
    imports?: readonly ts.StringLiteralLike[];
    moduleAugmentations?: readonly (ts.StringLiteral | ts.Identifier)[];
  };
  const usages = new Map<string, ts.StringLiteralLike[]>();
  const add = (literal: ts.StringLiteralLike): void => {
    usages.set(literal.text, [...(usages.get(literal.text) ?? []), literal]);
  };
  for (const literal of internal.imports ?? []) add(literal);
  for (const literal of internal.moduleAugmentations ?? []) {
    if (ts.isStringLiteral(literal)) add(literal);
  }
  return usages;
}

/** Why a module name cannot be given one mode: see the header. */
const UNPLACED = Symbol('unplaced');

export const moduleFormatResolutionHost: ResolutionHostFactory = (host, getOptions) => {
  let cache: ts.ModuleResolutionCache | undefined;
  let cacheOptions: ts.CompilerOptions | undefined;
  const cacheFor = (options: ts.CompilerOptions): ts.ModuleResolutionCache => {
    if (!cache || cacheOptions !== options) {
      cache = ts.createModuleResolutionCache(process.cwd(), (fileName) => fileName, options);
      cacheOptions = options;
    }
    return cache;
  };
  return {
    resolveModuleNames: (moduleNames, containingFile, _reusedNames, redirectedReference, options, containingSourceFile) => {
      const compilerOptions = options ?? getOptions();
      const resolutionCache = cacheFor(compilerOptions);
      if (
        containingSourceFile &&
        containingSourceFile.impliedNodeFormat === undefined &&
        formatDecidesMode(compilerOptions)
      ) {
        (containingSourceFile as { impliedNodeFormat?: ts.ResolutionMode }).impliedNodeFormat =
          ts.getImpliedNodeFormatForFile(
            containingFile,
            resolutionCache.getPackageJsonInfoCache(),
            host,
            compilerOptions
          );
      }
      const usages = containingSourceFile ? usagesOf(containingSourceFile) : undefined;
      const asked = new Map<string, number>();
      for (const name of moduleNames) asked.set(name, (asked.get(name) ?? 0) + 1);
      const seen = new Map<string, number>();
      const modeOf = (name: string): ts.ResolutionMode | typeof UNPLACED => {
        const occurrence = seen.get(name) ?? 0;
        seen.set(name, occurrence + 1);
        const literals = usages?.get(name) ?? [];
        if (!containingSourceFile || literals.length === 0) return undefined;
        const modes = literals.map((literal) =>
          ts.getModeForUsageLocation(containingSourceFile, literal, compilerOptions)
        );
        if (new Set(modes).size === 1) return modes[0];
        return asked.get(name) === literals.length ? modes[occurrence] : UNPLACED;
      };
      return moduleNames.map((name) => {
        const mode = modeOf(name);
        if (mode === UNPLACED) return undefined;
        return ts.resolveModuleName(
          name,
          containingFile,
          compilerOptions,
          host,
          resolutionCache,
          redirectedReference,
          mode
        ).resolvedModule;
      });
    },
  };
};
