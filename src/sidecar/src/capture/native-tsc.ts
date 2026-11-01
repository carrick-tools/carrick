/**
 * TypeScript 7's native `tsc` for two capture steps that ask the compiler no
 * question but "what does this program say" (carrick#3133): the declaration
 * emit of the surface entry and the self-check's diagnostics over the stub.
 * Everything that asks the checker a question (the anchors, the deep walk)
 * stays on TypeScript 6 through ts-morph.
 *
 * A command line takes no compiler host, so three things the in-process
 * programs get from a host are checked here instead, and any doubt hands the
 * step back to TypeScript 6, which then runs exactly as before:
 * - the options, written as a tsconfig: an option the native compiler rejects
 *   (TypeScript 7 removed `target: ES5`, `esModuleInterop: false`, `baseUrl`
 *   and others) is reported against that file, and the step falls back;
 * - the read bound of an untrusted tree (carrick#2230): every file the
 *   program read must be one the bound allows;
 * - the output: a diagnostic line the parser cannot read, an out-of-memory
 *   end or a signal is never read as "no diagnostics" (the #2951 lesson).
 */

import { spawnSync } from 'node:child_process';
import * as path from 'node:path';
import ts from 'typescript';
import { parseTscOutput, type RawDiagnostic } from './check-classify.js';
import { abnormalCause, compilerVersion, nativeMemoryLimit } from './check.js';
import type { ReadBound } from './checkout.js';

/** The native compiler a capture may run, and what it must keep to. */
export interface NativeCompiler {
  /** The native `tsc` executable. */
  tscPath: string;
  /** The untrusted-tree read bound, or undefined. */
  bound: ReadBound | undefined;
}

/** The native compiler's answer, or why it cannot be used. */
export type NativeRun =
  | {
      ok: true;
      /** Every diagnostic, with its file absolute. */
      diagnostics: Array<RawDiagnostic & { abs: string }>;
      /** The files the program read, absolute, when `--listFiles` or
       * `--listFilesOnly` was asked. */
      files: string[];
    }
  | { ok: false; reason: string };

/** In-process option keys with no tsconfig spelling; read, not written. */
const INTERNAL_OPTIONS = new Set(['configFilePath', 'pathsBasePath', 'configFile']);

interface OptionDeclaration {
  name: string;
  type: string | Map<string, unknown>;
  element?: { type: string | Map<string, unknown> };
}

function declarations(): Map<string, OptionDeclaration> {
  const list = (ts as unknown as { optionDeclarations: OptionDeclaration[] }).optionDeclarations;
  return new Map(list.map((d) => [d.name, d]));
}

/** The tsconfig spelling of an enum option's value (`ScriptTarget.ES2022` ->
 * `"es2022"`), or undefined when the value has none. */
function spellingOf(map: Map<string, unknown>, value: unknown): string | undefined {
  for (const [key, mapped] of map) if (mapped === value) return key;
  return undefined;
}

/**
 * `options` as a tsconfig's `compilerOptions`: enum values by name, `lib`
 * entries by their short name, and `paths` substitutions made absolute
 * against the directory they were relative to, so the file may sit anywhere.
 * `unconverted` names every option that could not be written; a caller that
 * gets any must not run the native compiler on the result.
 */
export function tsconfigCompilerOptions(options: ts.CompilerOptions): {
  compilerOptions: Record<string, unknown>;
  unconverted: string[];
} {
  const decls = declarations();
  const compilerOptions: Record<string, unknown> = {};
  const unconverted: string[] = [];
  for (const [name, value] of Object.entries(options)) {
    if (value === undefined || INTERNAL_OPTIONS.has(name)) continue;
    const decl = decls.get(name);
    if (!decl) {
      unconverted.push(name);
      continue;
    }
    if (decl.type instanceof Map) {
      const spelled = spellingOf(decl.type, value);
      if (spelled === undefined) unconverted.push(name);
      else compilerOptions[name] = spelled;
      continue;
    }
    if (decl.type === 'list' && decl.element?.type instanceof Map && Array.isArray(value)) {
      const map = decl.element.type;
      const spelled = value.map((v) => spellingOf(map, v));
      if (spelled.some((v) => v === undefined)) unconverted.push(name);
      else compilerOptions[name] = spelled;
      continue;
    }
    compilerOptions[name] = value;
  }
  if (options.paths) {
    const base = options.baseUrl ?? options.pathsBasePath;
    if (typeof base !== 'string') {
      unconverted.push('paths');
    } else {
      compilerOptions.paths = Object.fromEntries(
        Object.entries(options.paths).map(([pattern, targets]) => [
          pattern,
          targets.map((target) => path.resolve(base, target)),
        ])
      );
    }
  }
  return { compilerOptions, unconverted };
}

/**
 * Run the native compiler on `configPath` from `cwd`, with `extra` flags.
 * Synchronous, as the capture is: the in-process TypeScript 6 step it stands
 * in for blocks the same way.
 */
export function runNativeTsc(
  compiler: NativeCompiler,
  configPath: string,
  cwd: string,
  extra: string[] = []
): NativeRun {
  const memoryLimit = nativeMemoryLimit(process.execArgv, process.memoryUsage().rss / (1024 * 1024));
  const run = spawnSync(compiler.tscPath, ['-p', configPath, '--pretty', 'false', ...extra], {
    cwd,
    encoding: 'utf8',
    maxBuffer: 1024 * 1024 * 1024,
    env: memoryLimit ? { ...process.env, GOMEMLIMIT: memoryLimit } : process.env,
  });
  if (run.error) return { ok: false, reason: `the native compiler did not run: ${run.error.message}` };
  if (run.signal) return { ok: false, reason: `the native compiler ended on ${run.signal}` };
  const output = `${run.stdout}\n${run.stderr}`;
  const abnormal = abnormalCause(output);
  if (abnormal) return { ok: false, reason: abnormal };
  const parsed = parseTscOutput(output);
  // A diagnostic with no (line,col) prefix (a global one, such as TS18003) is
  // not parsed: counted here so that it can never read as a clean program.
  const errorLines = output.split('\n').filter((line) => /\berror TS\d+:/.test(line));
  if (errorLines.length !== parsed.length) {
    const fileless = errorLines.find((line) => !/\(\d+,\d+\): error TS\d+:/.test(line)) ?? '';
    return { ok: false, reason: `the native compiler printed a diagnostic with no file: ${fileless.trim()}` };
  }
  const configAbs = path.resolve(cwd, configPath);
  const diagnostics = parsed.map((d) => ({ ...d, abs: path.resolve(cwd, d.file) }));
  const config = diagnostics.find((d) => d.abs === configAbs || path.basename(d.abs) === path.basename(configAbs));
  if (config) return { ok: false, reason: `the native compiler refused the options: TS${config.code} ${config.message}` };
  const files = run.stdout
    .split('\n')
    .map((line) => line.replace(/\r$/, ''))
    .filter((line) => path.isAbsolute(line) && !/\): error TS\d+: /.test(line));
  if (compiler.bound) {
    const bound = compiler.bound;
    const outside = files.find((file) => !isNativeLibFile(compiler, file) && !bound.allows(file));
    if (outside) return { ok: false, reason: `the program read ${outside}, outside the read bound` };
  }
  return { ok: true, diagnostics, files };
}

/** Which native compiler a path is, for the run log. */
export function nativeLabel(compiler: NativeCompiler): string {
  return `TypeScript ${compilerVersion(compiler.tscPath)} (native)`;
}

/** A tsconfig's text: these options over exactly these files. */
export function tsconfigText(compilerOptions: Record<string, unknown>, files: string[]): string {
  return JSON.stringify({ compilerOptions, files }, null, 2) + '\n';
}

/** Whether `file` is a native compiler's own default library file. */
export function isNativeLibFile(compiler: NativeCompiler, file: string): boolean {
  return file.startsWith(path.dirname(compiler.tscPath) + path.sep);
}

/**
 * The two TypeScript 6 calls the native compiler stands in for, called
 * through this object so that a test can count them (carrick#3133): a
 * capture the native compiler served makes neither.
 */
export const typescript6Steps = {
  preEmitDiagnostics: (program: ts.Program): readonly ts.Diagnostic[] => ts.getPreEmitDiagnostics(program),
  emit: (program: ts.Program, writeFile: ts.WriteFileCallback): ts.EmitResult =>
    program.emit(
      undefined,
      (fileName, text, bom, onError, sources, data) => writeFile(fileName, text, bom, onError, sources, data),
      undefined,
      /* emitOnlyDtsFiles */ true
    ),
};
