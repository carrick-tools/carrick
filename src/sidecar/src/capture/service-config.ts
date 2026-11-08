/**
 * Which tsconfig types a service that names none (carrick#1776).
 *
 * Both readers of a service, the init'd project and capture, ask this one
 * function, so they type the service under the same options.
 *
 * The service root is searched first, for the names a service keeps its own
 * config under. Above it, the search is `tsc`'s own when it is run with no
 * `-p`: the nearest `tsconfig.json` in an ancestor directory. A layout that
 * keeps one config above several services (their module resolution, `paths`,
 * `lib`) is typed under that config, as the repo's own `tsc` types it; without
 * it, every import only that config resolves read `any`.
 *
 * A service-root `jsconfig.json` ranks after those names and before an
 * ancestor `tsconfig.json` (carrick#3497): tsserver types a JavaScript file
 * under the jsconfig beside it, not under a config above. Where no ancestor
 * config exists the answer stays undefined, as before: each file is then
 * typed by `GoverningConfigs` alone, which reads the same jsconfig.
 *
 * The walk stops at the scan root, inclusive: a config outside the scanned
 * tree is never read. With no scan root, or a service outside it, only the
 * service root is searched.
 *
 * The service config is the fallback of the per-file rule
 * (`GoverningConfigs`, carrick#3497): a file a config inside the service
 * lists is typed under that config.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import { ProjectGraph } from './project-references.js';

/** The names a service root is searched for, in order. */
const SERVICE_ROOT_NAMES = ['tsconfig.json', 'tsconfig.build.json', 'tsconfig.app.json'];

/** The root config tsserver types a JavaScript service under, after the names above. */
const SERVICE_ROOT_JSCONFIG = 'jsconfig.json';

/** The name an ancestor directory is searched for, as `tsc` searches. */
const ANCESTOR_NAME = 'tsconfig.json';

/** Absolute path of the config that types the service, or undefined for none. */
export function findServiceTsconfig(serviceRoot: string, scanRoot?: string): string | undefined {
  const root = path.resolve(serviceRoot);
  for (const name of SERVICE_ROOT_NAMES) {
    const candidate = path.join(root, name);
    if (fs.existsSync(candidate)) return candidate;
  }
  const jsconfig = path.join(root, SERVICE_ROOT_JSCONFIG);
  const ownJsconfig = fs.existsSync(jsconfig) ? jsconfig : undefined;
  if (scanRoot === undefined) return undefined;
  // Containment is decided on real paths, so a symlink on either end cannot
  // hide the service inside the scan root or place it outside; the walk then
  // climbs the path as given, one directory per level between them.
  const fromBound = path.relative(realPath(scanRoot), realPath(root));
  if (fromBound === '' || fromBound.startsWith('..') || path.isAbsolute(fromBound)) return undefined;
  let dir = root;
  for (let level = fromBound.split(path.sep).length; level > 0; level--) {
    dir = path.dirname(dir);
    const candidate = path.join(dir, ANCESTOR_NAME);
    if (fs.existsSync(candidate)) return ownJsconfig ?? candidate;
  }
  return undefined;
}

/** The path with every symlink resolved; the path as given when it does not exist. */
export function realPath(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}

/** The names an editor looks for in a file's directory, in order. */
const FILE_CONFIG_NAMES = ['tsconfig.json', 'jsconfig.json'];

/**
 * The config each file of one service is typed under (carrick#1604,
 * carrick#3497 ruling R-N1): the one the TypeScript language service picks.
 * From the file's directory up to the service root, inclusive, each directory's
 * `tsconfig.json`, then the `jsconfig.json` beside it, is asked whether its
 * program, or a project it references, lists the file; the first that does
 * types it (a
 * solution config hands the file to the reference that lists it). A config
 * that lists the file nowhere is passed over. The climb stops after a
 * directory named `node_modules`, as tsserver's does. A file no config inside
 * the service lists falls back to the service config (`findServiceTsconfig`
 * or the named one) and the project of its references that lists the file,
 * else to that config itself; with no service config, to none.
 *
 * The scanner's `module_aliases::Governing` is the same rule, and
 * `tests/module_resolution_parity_test.rs` holds both to the compiler's own
 * answer.
 */
export class GoverningConfigs {
  private readonly root: string;
  /** Directory -> the configs in it, in order, for every directory a climb visited. */
  private readonly dirConfigs = new Map<string, string[]>();
  /** Config -> its projects; undefined for one that cannot be read. */
  private readonly graphs = new Map<string, { graph: ProjectGraph; reported: number } | undefined>();

  /**
   * @param serviceRoot the service directory
   * @param serviceConfig the fallback config, absolute, or undefined for none
   * @param report where a config or reference that cannot be read is named
   */
  constructor(
    serviceRoot: string,
    private readonly serviceConfig: string | undefined,
    private readonly report: (diagnostic: string) => void = () => {}
  ) {
    this.root = path.resolve(serviceRoot);
  }

  /** Absolute path of the config that types `file`, or undefined for none. */
  configFor(file: string): string | undefined {
    const absolute = path.resolve(file);
    let dir = path.dirname(absolute);
    for (;;) {
      const from = path.relative(this.root, dir);
      if (from.startsWith('..') || path.isAbsolute(from)) break;
      for (const config of this.configsIn(dir)) {
        const owner = this.listingOwner(config, absolute);
        if (owner !== undefined) return owner;
      }
      // tsserver looks in a `node_modules` directory itself, then stops.
      if (dir === this.root || path.basename(dir) === 'node_modules') break;
      dir = path.dirname(dir);
    }
    if (this.serviceConfig === undefined) return undefined;
    return this.listingOwner(this.serviceConfig, absolute) ?? this.serviceConfig;
  }

  private configsIn(dir: string): string[] {
    let configs = this.dirConfigs.get(dir);
    if (configs === undefined) {
      configs = FILE_CONFIG_NAMES.map((n) => path.join(dir, n)).filter((c) => fs.existsSync(c));
      this.dirConfigs.set(dir, configs);
    }
    return configs;
  }

  /** The project reached from `config` that lists `file`, absolute. */
  private listingOwner(config: string, file: string): string | undefined {
    if (!this.graphs.has(config)) {
      try {
        this.graphs.set(config, { graph: new ProjectGraph(config), reported: 0 });
      } catch (err) {
        this.report(`tsconfig ${config} could not be read and was skipped: ${err instanceof Error ? err.message : String(err)}`);
        this.graphs.set(config, undefined);
      }
    }
    const entry = this.graphs.get(config);
    if (entry === undefined) return undefined;
    const owner = entry.graph.listingOwnerOf(file);
    entry.graph.diagnostics.slice(entry.reported).forEach(this.report);
    entry.reported = entry.graph.diagnostics.length;
    return owner?.configPath;
  }
}
