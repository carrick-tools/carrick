/**
 * Shared test helpers: sidecar process client and fixture paths.
 */
import type { Project } from 'ts-morph';
import type { ExpandOrigin } from '../src/type-structural-expander.js';
export declare const SIDECAR_PATH: string;
export declare const FIXTURES_PATH: string;
/**
 * Write .d.ts content into a minimal capture-stub-shaped temp dir
 * (`<dir>/types/surface.d.ts`) for the stub-based `resolve_definitions`
 * action. Callers own cleanup (or leave it to the OS temp dir).
 */
export declare function stubDirFor(dts: string): string;
/**
 * The `ExpandOrigin` for a test project: the structural printer decides
 * library origin from the program, so every caller has to hand it one.
 * `repoRoot` defaults to the root, which is what an in-memory project's files
 * sit under.
 */
export declare function expandOriginOf(project: Project, repoRoot?: string): ExpandOrigin;
/** A gzipped ustar tarball laid out as npm publishes one: every path under `package/`. */
export declare function tgz(files: Record<string, string>): Buffer;
export declare function integrityOf(bytes: Buffer): string;
export interface RegistryPackage {
    name: string;
    version: string;
    /** Merged into the published package.json beside name and version. */
    manifest?: Record<string, unknown>;
    files?: Record<string, string>;
    /** Listed in the packument, but its tarball answers 404. */
    unfetchable?: boolean;
}
export interface LocalRegistry {
    /** Base URL with a trailing slash, for `npm_config_registry`. */
    url: string;
    /** Tarball paths requested, in order. */
    tarballs: string[];
    close(): Promise<void>;
}
/**
 * An npm registry on 127.0.0.1 serving packuments and tarballs for the given
 * packages, so an installer runs offline against a fixed set of versions.
 */
export declare function localRegistry(packages: RegistryPackage[]): Promise<LocalRegistry>;
/**
 * Point every installer this process spawns at `registry`, with a store and
 * cache under `scratch`, so the developer's own store is never read. Returns
 * the restore.
 */
export declare function useRegistry(registry: LocalRegistry, scratch: string): () => void;
/**
 * Helper class to manage sidecar process communication
 */
export declare class SidecarClient {
    private process;
    private responseBuffer;
    private responsePromises;
    /**
     * Start the sidecar process, with `env` added to this process's environment
     */
    start(env?: NodeJS.ProcessEnv): Promise<void>;
    /**
     * Send a request and wait for response
     */
    send<T = unknown>(request: Record<string, unknown>, timeoutMs?: number): Promise<T>;
    /**
     * Stop the sidecar process
     */
    stop(): Promise<void>;
}
/** A decompressed rows file as version 1 wrote it, before the publication bits (carrick#2508). */
export declare function asVersion1Rows(text: string): string;
/** A decompressed rows file as version 2 wrote it. */
export declare function asVersion2Rows(text: string): string;
/**
 * A `list_library_surface` surface with every handler parameter's `type_id`
 * resolved back to the `type` and `members` of its `handler_types` entry
 * (carrick#2977), the shape the lister wrote inline before the table, and
 * `handler_types`, `types` and `handler_params_left_out` dropped. `dropped`
 * sums each referring parameter's entry `dropped`: what the inline lister
 * counted as `handlers_truncated` when its budget left nothing out.
 */
export declare function inlineHandlerParams(surface: Record<string, unknown>): {
    surface: Record<string, unknown>;
    dropped: number;
};
