/**
 * Which project types a file of a service whose tsconfig references others
 * (carrick#1604).
 *
 * A "solution" tsconfig (`"files": []` plus `references`) carries no compiler
 * options: they live in the projects it references. Parsing the named file
 * alone typed every file with none, so `customConditions`, `paths` and the
 * module resolution mode never applied and workspace imports fell through to
 * unbuilt output.
 *
 * TypeScript's editor answers "which project types this file" by searching
 * the named config, then its references depth-first in declared order, for
 * the first project whose file list includes the file. That project is the
 * file's owner here too, and a file is only ever typed under its owner's
 * options: the loader builds one program per owner it is asked about, and
 * the capture types each anchor in its owner's program. A file no reachable
 * project includes is owned by the named config, which is how it was typed
 * before references were read.
 */

import * as path from 'node:path';
import ts from 'typescript';

/** One parsed tsconfig. */
export interface ServiceProject {
  /** Absolute path of the config file the options came from. */
  configPath: string;
  parsed: ts.ParsedCommandLine;
}

/** Parse one config; throws on a file that cannot be read or parsed. */
function parseConfig(
  configPath: string,
  extendedConfigCache: Map<string, ts.ExtendedConfigCacheEntry>
): ServiceProject {
  const host: ts.ParseConfigFileHost = {
    ...ts.sys,
    onUnRecoverableConfigFileDiagnostic: (d) => {
      throw new Error(ts.flattenDiagnosticMessageText(d.messageText, '\n'));
    },
  };
  const parsed = ts.getParsedCommandLineOfConfigFile(configPath, {}, host, extendedConfigCache);
  if (!parsed) throw new Error(`failed to parse ${configPath}`);
  return { configPath: path.resolve(configPath), parsed };
}

const fileKey = (file: string): string => path.resolve(file);

/**
 * The projects a named config reaches and the owner of each file among them.
 * References are read only when a file the named config does not include is
 * asked about, so a service whose files the named config includes costs one
 * parse, as before.
 */
export class ProjectGraph {
  readonly named: ServiceProject;
  /** References that could not be read, one line each. */
  readonly diagnostics: string[] = [];
  private readonly extendedConfigCache = new Map<string, ts.ExtendedConfigCacheEntry>();
  private walked: { project: ServiceProject; files: Set<string> }[] | undefined;
  private readonly namedFiles: Set<string>;

  /** Throws when the named config cannot be read, as a bare parse would. */
  constructor(configPath: string) {
    this.named = parseConfig(configPath, this.extendedConfigCache);
    this.namedFiles = new Set(this.named.parsed.fileNames.map(fileKey));
  }

  /** Whether the named config references other projects at all. */
  get hasReferences(): boolean {
    return (this.named.parsed.projectReferences?.length ?? 0) > 0;
  }

  /**
   * The project that owns `file`: the first in search order whose file list
   * includes it (the named config, then its references depth-first in
   * declared order; a config reached twice is visited once). The named config
   * when none does.
   */
  ownerOf(file: string): ServiceProject {
    const key = fileKey(file);
    if (this.namedFiles.has(key) || !this.hasReferences) return this.named;
    return this.projects().find((entry) => entry.files.has(key))?.project ?? this.named;
  }

  /** Position in search order, 0 for the named config. */
  rank(project: ServiceProject): number {
    if (project === this.named) return 0;
    const index = this.projects().findIndex((entry) => entry.project === project);
    return index === -1 ? Number.MAX_SAFE_INTEGER : index;
  }

  private projects(): { project: ServiceProject; files: Set<string> }[] {
    if (this.walked) return this.walked;
    const visited = new Set<string>([this.named.configPath]);
    const order: { project: ServiceProject; files: Set<string> }[] = [];
    const visit = (project: ServiceProject, files: Set<string>): void => {
      order.push({ project, files });
      for (const reference of project.parsed.projectReferences ?? []) {
        const referencePath = path.resolve(ts.resolveProjectReferencePath(reference));
        if (visited.has(referencePath)) continue;
        visited.add(referencePath);
        let child: ServiceProject;
        try {
          child = parseConfig(referencePath, this.extendedConfigCache);
        } catch (err) {
          this.diagnostics.push(
            `referenced tsconfig ${referencePath} (from ${project.configPath}) could not be read and was skipped: ` +
              (err instanceof Error ? err.message : String(err))
          );
          continue;
        }
        visit(child, new Set(child.parsed.fileNames.map(fileKey)));
      }
    };
    visit(this.named, this.namedFiles);
    this.walked = order;
    return order;
  }
}

/**
 * For the loader: the config each file of the service is typed under. The
 * returned function gives the owner's config path for a file, and the named
 * config for no file; a reference that cannot be read goes to `report`. A named config that references nothing is not parsed
 * here (`references` is never inherited through `extends`), so the function
 * answers the named path for every file and the common case costs nothing.
 */
export function serviceConfigPath(
  configPath: string,
  report: (diagnostic: string) => void = () => {}
): (file?: string) => string {
  const raw = ts.readConfigFile(configPath, ts.sys.readFile).config as
    | { references?: unknown }
    | undefined;
  if (!Array.isArray(raw?.references) || raw.references.length === 0) {
    return () => configPath;
  }
  let graph: ProjectGraph | undefined;
  return (file) => {
    if (file === undefined) return configPath;
    graph ??= new ProjectGraph(configPath);
    const reported = graph.diagnostics.length;
    const owner = graph.ownerOf(file);
    graph.diagnostics.slice(reported).forEach(report);
    return owner === graph.named ? configPath : owner.configPath;
  };
}

/** Compiler options that only place output, so two projects differing only in these emit the same declarations. */
const OUTPUT_ONLY_OPTIONS = new Set([
  'rootDir',
  'outDir',
  'declarationDir',
  'outFile',
  'composite',
  'declaration',
  'declarationMap',
  'emitDeclarationOnly',
  'incremental',
  'tsBuildInfoFile',
  'configFilePath',
  'noEmit',
  'sourceMap',
  'inlineSourceMap',
  'inlineSources',
]);

/**
 * Whether two projects' options would emit a declaration the same way, so a
 * type one of them names can be emitted by the other.
 */
export function emitsAlike(a: ServiceProject, b: ServiceProject): boolean {
  const relevant = (options: ts.CompilerOptions): string =>
    JSON.stringify(
      Object.keys(options)
        .filter((key) => !OUTPUT_ONLY_OPTIONS.has(key))
        .sort()
        .map((key) => [key, options[key]])
    );
  return relevant(a.parsed.options) === relevant(b.parsed.options);
}
