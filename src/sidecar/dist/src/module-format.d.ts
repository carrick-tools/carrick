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
import { ts, type ResolutionHost } from 'ts-morph';
/**
 * A resolution host that also hands the compiler a module resolution cache.
 * ts-morph's `ResolutionHost` has no such member, and its `createHosts`
 * copies only the three it declares onto the compiler host, so a host with
 * its own `resolveModuleNames` left every program without a cache: the
 * compiler then read the `package.json` scope above every file again on
 * every rebuild (37% of a retyped call in a large program).
 */
export interface CachingResolutionHost extends ResolutionHost {
    getModuleResolutionCache(): ts.ModuleResolutionCache;
}
/**
 * The editor's project-reference rule for the program's owner config
 * (`ReferenceRedirect` in `capture/reference-redirect.ts`, carrick#2418),
 * restated here because this module is outside the capture seam.
 */
export interface ResolutionRedirect {
    referenceFor(containingFile: string): unknown;
    resolutionHost<H extends {
        fileExists(fileName: string): boolean;
    }>(host: H): H;
    sourceAnswer<R extends {
        resolvedFileName: string;
    } | undefined>(answer: R): R;
}
/**
 * The resolution host factory, resolving through `redirect` when the owner
 * config references other projects.
 */
export declare const moduleFormatResolutionHost: (redirect?: ResolutionRedirect) => (host: ts.ModuleResolutionHost, getOptions: () => ts.CompilerOptions) => CachingResolutionHost;
