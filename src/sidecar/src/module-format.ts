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
 * are resolved, then resolves each one as the compiler's default does:
 * `ts.resolveModuleName` in the mode `ts.getModeForUsageLocation` reads off
 * the import. Under any other `module` setting the compiler does not read a
 * file's format and the mode comes out as it did before.
 */

import { ts, type ResolutionHostFactory } from 'ts-morph';

/** Whether the compiler decides an import's mode from the importing file's format. */
function formatDecidesMode(options: ts.CompilerOptions): boolean {
  const kind = options.module;
  return kind !== undefined && kind >= ts.ModuleKind.Node16 && kind <= ts.ModuleKind.NodeNext;
}

/**
 * The literals in `file` the program resolves, by module name: its imports and
 * its module augmentations. Neither list is in the public declaration file.
 * The first literal wins when one name is imported twice.
 */
function usagesOf(file: ts.SourceFile): Map<string, ts.StringLiteralLike> {
  const internal = file as ts.SourceFile & {
    imports?: readonly ts.StringLiteralLike[];
    moduleAugmentations?: readonly (ts.StringLiteral | ts.Identifier)[];
  };
  const usages = new Map<string, ts.StringLiteralLike>();
  for (const literal of internal.imports ?? []) {
    if (!usages.has(literal.text)) usages.set(literal.text, literal);
  }
  for (const literal of internal.moduleAugmentations ?? []) {
    if (ts.isStringLiteral(literal) && !usages.has(literal.text)) usages.set(literal.text, literal);
  }
  return usages;
}

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
      return moduleNames.map((name) => {
        const usage = usages?.get(name);
        const mode =
          containingSourceFile && usage
            ? ts.getModeForUsageLocation(containingSourceFile, usage, compilerOptions)
            : undefined;
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
