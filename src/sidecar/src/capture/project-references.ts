/**
 * Which tsconfig a service's program is built from when the one it names
 * lists no files of its own (carrick#1604).
 *
 * A "solution" tsconfig (`"files": []` plus `references`) carries no compiler
 * options: they live in the projects it references. Parsing the named file
 * alone built the program with none, so `customConditions`, `paths` and the
 * module resolution mode never applied and workspace imports fell through to
 * unbuilt output.
 *
 * TypeScript's editor answers "which project types this file" by searching
 * the named config, then its references depth-first in declared order, for
 * the first project whose file list includes the file. `projectForFiles`
 * applies that rule to every file being typed and builds from the project
 * that owns the most of them, because a program has one set of options.
 *
 * Shared by the capture (anchor files known) and the ts-morph project loader
 * (the service's files).
 */

import * as path from 'node:path';
import ts from 'typescript';

/** One parsed tsconfig. */
export interface ServiceProject {
  /** Absolute path of the config file the options came from. */
  configPath: string;
  parsed: ts.ParsedCommandLine;
}

/** The project a program is built from, and what the choice skipped over. */
export interface ProjectChoice {
  project: ServiceProject;
  /**
   * One line per reference that could not be read and per file typed under
   * options that are not its own project's. Empty when the named config
   * includes every file.
   */
  diagnostics: string[];
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

/**
 * Parse the named config. Throws when it cannot be read, as a bare parse
 * would; a reference that cannot be read is reported by `projectForFiles`
 * instead.
 */
export function parseNamedConfig(configPath: string): ServiceProject {
  return parseConfig(configPath, new Map());
}

/**
 * Every project `named` reaches, in the order the editor searches them: the
 * named config first, then each reference depth-first in declared order. A
 * config reached twice (a cycle, or a diamond) is visited once.
 */
function reachableProjects(
  named: ServiceProject,
  diagnostics: string[]
): ServiceProject[] {
  const cache = new Map<string, ts.ExtendedConfigCacheEntry>();
  const visited = new Set<string>([named.configPath]);
  const order: ServiceProject[] = [];
  const visit = (project: ServiceProject): void => {
    order.push(project);
    for (const reference of project.parsed.projectReferences ?? []) {
      const referencePath = path.resolve(ts.resolveProjectReferencePath(reference));
      if (visited.has(referencePath)) continue;
      visited.add(referencePath);
      let child: ServiceProject;
      try {
        child = parseConfig(referencePath, cache);
      } catch (err) {
        diagnostics.push(
          `referenced tsconfig ${referencePath} (from ${project.configPath}) could not be read and was skipped: ` +
            (err instanceof Error ? err.message : String(err))
        );
        continue;
      }
      visit(child);
    }
  };
  visit(named);
  return order;
}

const fileKey = (file: string): string => path.resolve(file);

/**
 * Among `projects` (search order, `named` first), the one that includes the
 * most of `files`, each file counted for the first project that includes it;
 * ties go to search order, and `named` when none includes any of them.
 */
function chooseAmong(
  named: ServiceProject,
  projects: readonly ServiceProject[],
  files: readonly string[],
  diagnostics: string[]
): ServiceProject {
  const sets = projects.map((project) => new Set(project.parsed.fileNames.map(fileKey)));
  const owned = new Map<ServiceProject, string[]>();
  const unowned: string[] = [];
  for (const file of new Set(files.map(fileKey))) {
    const index = sets.findIndex((set) => set.has(file));
    if (index === -1) {
      unowned.push(file);
      continue;
    }
    const owner = projects[index];
    owned.set(owner, [...(owned.get(owner) ?? []), file]);
  }

  let chosen = named;
  let most = 0;
  for (const project of projects) {
    const count = owned.get(project)?.length ?? 0;
    if (count > most) {
      chosen = project;
      most = count;
    }
  }

  const relative = (file: string): string =>
    path.relative(path.dirname(named.configPath), file).split(path.sep).join('/');
  for (const [project, members] of owned) {
    if (project === chosen) continue;
    diagnostics.push(
      `${members.length} file(s) belong to ${project.configPath} and are typed under ` +
        `${chosen.configPath}: ${members.map(relative).join(', ')}`
    );
  }
  if (unowned.length > 0) {
    diagnostics.push(
      `no project reachable from ${named.configPath} includes ${unowned.map(relative).join(', ')}; ` +
        `typed under ${chosen.configPath}`
    );
  }
  return chosen;
}

const hasReferences = (project: ServiceProject): boolean =>
  (project.parsed.projectReferences?.length ?? 0) > 0;

/**
 * The project to build a program for `files` from: the one that includes the
 * most of them, each file counted for the first project in search order that
 * includes it (TypeScript's rule); ties go to search order. The named config
 * when it includes every file or references nothing (no reference is read
 * then), and when no project includes any of them.
 */
export function projectForFiles(named: ServiceProject, files: readonly string[]): ProjectChoice {
  const namedFiles = new Set(named.parsed.fileNames.map(fileKey));
  if (!hasReferences(named) || files.every((file) => namedFiles.has(fileKey(file)))) {
    return { project: named, diagnostics: [] };
  }
  const diagnostics: string[] = [];
  const projects = reachableProjects(named, diagnostics);
  return { project: chooseAmong(named, projects, files, diagnostics), diagnostics };
}

/**
 * The config the whole service's program is built from, before any one file
 * is typed: the named config when it includes files of its own or references
 * nothing; otherwise the reachable project that includes the most of the
 * service directory's files. A config that references nothing is not parsed
 * here (`references` is never inherited through `extends`), so the common
 * case costs nothing beyond the read the caller does anyway.
 */
export function serviceConfigPath(
  configPath: string,
  serviceRoot: string
): { configPath: string; diagnostics: string[] } {
  const raw = ts.readConfigFile(configPath, ts.sys.readFile).config as
    | { references?: unknown }
    | undefined;
  if (!Array.isArray(raw?.references) || raw.references.length === 0) {
    return { configPath, diagnostics: [] };
  }
  const named = parseNamedConfig(configPath);
  if (named.parsed.fileNames.length > 0) {
    return { configPath, diagnostics: [] };
  }
  const diagnostics: string[] = [];
  const projects = reachableProjects(named, diagnostics);
  const root = fileKey(serviceRoot) + path.sep;
  const serviceFiles = projects.flatMap((project) =>
    project.parsed.fileNames
      .map(fileKey)
      .filter((file) => file.startsWith(root) && !file.includes(`${path.sep}node_modules${path.sep}`))
  );
  const chosen = chooseAmong(named, projects, serviceFiles, diagnostics);
  return { configPath: chosen.configPath, diagnostics };
}
