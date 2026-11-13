/**
 * How a stub's module specifiers resolve: one answer for the capture's
 * self-check and for the definition resolver that reads the stub after it
 * (carrick#3995 T5).
 *
 * The stub is written outside the checkout, so a bare specifier in it
 * (`import("pkg").Thing`) reaches nothing by itself. The self-check reaches the
 * producer's installed packages three ways: a `node_modules` link inside the
 * stub to the producer's own, the capture's compiler host (installed-package
 * rewrites, a Deno graph, the untrusted-tree read bound), and, for a
 * declaration emitted from a source outside the service's rootDir, that
 * source's own place in the checkout. The resolver used to read the stub with
 * none of them, so an alias the self-check called clean answered `any`.
 *
 * The capture remembers each stub's resolution in this process; the resolver
 * asks for it by stub directory. The compiler engine runs a capture in a
 * process of its own and resolves its stub in the same process, right after.
 * A stub this process did not capture reads with nothing installed, as before.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import type { WriteGuard } from './guarded-fs.js';

/** What a stub's resolution is built from: the capture's, per stub. */
export interface StubResolution {
  /** Writes held to this stub. */
  guard: WriteGuard;
  stubDir: string;
  bareCheckout: boolean;
  /** Producer repo root; its node_modules (if any) backs resolution. */
  repoRoot: string;
  compilerHost?: (options: ts.CompilerOptions) => ts.CompilerHost;
  /**
   * Absolute stub path of each declaration emitted for a source outside the
   * service's rootDir -> that source's own path. A bare specifier such a
   * declaration names that the stub's links do not reach is resolved from the
   * source's place in the checkout, as the compiler resolved it at emit.
   */
  outsideSources?: Readonly<Record<string, string>>;
}

/**
 * Point the stub at the producer's installed packages: a `node_modules` link
 * in the stub to the producer's own, unless the checkout is bare or the stub
 * already has one. Returns what removes the link again.
 */
export function linkInstalledPackages(resolution: StubResolution): () => void {
  const repoNodeModules = path.join(resolution.repoRoot, 'node_modules');
  const linkPath = path.join(resolution.stubDir, 'node_modules');
  if (resolution.bareCheckout || !fs.existsSync(repoNodeModules) || fs.existsSync(linkPath)) return () => {};
  resolution.guard.symlink(repoNodeModules, linkPath, 'dir');
  // unlinkSync, not rmSync: the link target is a directory and rmSync
  // refuses symlinks-to-directories with EISDIR.
  return () => resolution.guard.unlink(linkPath);
}

/**
 * The compiler host of a program over the stub: the capture's, with outside
 * declarations resolving from their source. `undefined` means the compiler's
 * default host.
 */
export function stubResolutionHost(
  resolution: StubResolution,
  options: ts.CompilerOptions
): ts.CompilerHost | undefined {
  const outside = resolution.outsideSources;
  if (!outside || Object.keys(outside).length === 0) return resolution.compilerHost?.(options);
  const host = resolution.compilerHost?.(options) ?? ts.createCompilerHost(options);
  const fallback = host.resolveModuleNames?.bind(host);
  host.resolveModuleNames = (names, containingFile, reused, redirected, compilerOptions, containingSourceFile) =>
    names.map((name, index) => {
      const resolved = fallback
        ? fallback([name], containingFile, reused?.slice(index, index + 1), redirected, compilerOptions, containingSourceFile)[0]
        : ts.resolveModuleName(name, containingFile, compilerOptions, host).resolvedModule;
      const source = outside[path.resolve(containingFile)];
      if (resolved || !source || name.startsWith('.') || path.isAbsolute(name)) return resolved;
      return ts.resolveModuleName(name, source, compilerOptions, host).resolvedModule;
    });
  return host;
}

/** Each stub this process captured, by absolute stub directory. */
const captured = new Map<string, StubResolution>();

/** Forget every stub of an earlier capture: only the latest is resolved. */
export function forgetStubResolutions(): void {
  captured.clear();
}

/** Remember how a captured stub resolves, for the resolver that reads it next. */
export function rememberStubResolution(resolution: StubResolution): void {
  captured.set(path.resolve(resolution.stubDir), resolution);
}

/** Resolves module names in a program over a stub. */
export type StubModuleResolver = (
  moduleNames: string[],
  containingFile: string,
  options: ts.CompilerOptions
) => (ts.ResolvedModuleFull | undefined)[];

/**
 * Run `use` with the stub resolving as its self-check did: the installed
 * packages linked in, and module names resolved through the self-check's host.
 * `use` gets `undefined` when this process did not capture the stub; the stub
 * then reads with nothing installed.
 */
export function withStubResolution<T>(stubDir: string, use: (resolve: StubModuleResolver | undefined) => T): T {
  const resolution = captured.get(path.resolve(stubDir));
  if (!resolution) return use(undefined);
  const unlink = linkInstalledPackages(resolution);
  try {
    const hosts = new Map<ts.CompilerOptions, ts.CompilerHost>();
    const hostOf = (options: ts.CompilerOptions): ts.CompilerHost => {
      let host = hosts.get(options);
      if (!host) {
        host = stubResolutionHost(resolution, options) ?? ts.createCompilerHost(options);
        hosts.set(options, host);
      }
      return host;
    };
    return use((moduleNames, containingFile, options) => {
      const host = hostOf(options);
      if (host.resolveModuleNames) {
        return host.resolveModuleNames(moduleNames, containingFile, undefined, undefined, options) as (
          | ts.ResolvedModuleFull
          | undefined
        )[];
      }
      return moduleNames.map((name) => ts.resolveModuleName(name, containingFile, options, host).resolvedModule);
    });
  } finally {
    unlink();
  }
}
