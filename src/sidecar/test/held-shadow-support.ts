/** Shared by the write_shadow tests: one sidecar process per call, and the two compilers' own-file errors. */
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { ProjectLoader } from '../src/project-loader.js';
import { judgeTscPath } from '../src/capture/check.js';
import { SIDECAR_PATH } from './helpers.js';

export interface Shadow {
  status: string;
  errors?: string[];
  tsconfig: string;
  own_files: string[];
  vendored: number;
  unresolved: number;
  path_conflicts: unknown[];
  bare_specifiers: string[];
  hono: boolean;
  native_tsc: string | null;
}

/** One sidecar process, started in `cwd`: init on `repo`, then write_shadow into `out`. */
export function writeShadow(repo: string, out: string, cwd: string = os.tmpdir()): Shadow {
  const lines = [
    { action: 'init', request_id: 'i', repo_root: repo, scan_root: repo },
    { action: 'write_shadow', request_id: 's', out_dir: out },
    { action: 'shutdown', request_id: 'x' },
  ];
  const run = spawnSync('node', [SIDECAR_PATH], {
    cwd,
    input: lines.map((l) => JSON.stringify(l)).join('\n') + '\n',
    encoding: 'utf8',
    timeout: 120_000,
    maxBuffer: 64 * 1024 * 1024,
  });
  const answers = run.stdout
    .split('\n')
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l) as { request_id: string; status: string });
  const shadow = answers.find((a) => a.request_id === 's');
  assert.ok(shadow, `no write_shadow answer: ${run.stderr.slice(-2000)}`);
  assert.equal(shadow.status, 'success', JSON.stringify(shadow));
  return shadow as unknown as Shadow;
}

export function scratch(prefix: string): string {
  return fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
}

export function nativeTsc(): string {
  const tsc = judgeTscPath();
  assert.match(tsc, /@typescript[\\/]typescript-/, `the native compiler is not installed: ${tsc}`);
  return tsc;
}

/** `file:line:code` of every error the native compiler reports for files under `own`. */
export function nativeErrors(tsconfig: string, own: string): string[] {
  const run = spawnSync(nativeTsc(), ['-p', tsconfig, '--pretty', 'false'], { cwd: own, encoding: 'utf8' });
  const found: string[] = [];
  for (const line of run.stdout.split('\n')) {
    // A config the compiler refuses (TS5108, TS6046) names no file: that is a failure, never a clean run.
    assert.doesNotMatch(line, /^error TS\d+/, `the compiler refused the config: ${line}`);
    const m = /^(.+?)\((\d+),\d+\): error (TS\d+)/.exec(line);
    if (!m) continue;
    const file = path.resolve(own, m[1]);
    if (!file.startsWith(own + path.sep)) continue;
    found.push(`${path.relative(own, file)}:${m[2]}:${m[3]}`);
  }
  return found.sort();
}

/** The same, from the loader's TypeScript 6 program, for the repo's own files. */
export function loaderErrors(repo: string): string[] {
  // The loader resolves `types` references from the working directory: read from a neutral one, as the held process does.
  const cwd = process.cwd();
  process.chdir(os.tmpdir());
  try {
    return loaderErrorsHere(repo);
  } finally {
    process.chdir(cwd);
  }
}

function loaderErrorsHere(repo: string): string[] {
  const loader = new ProjectLoader({ repoRoot: repo, scanRoot: repo });
  assert.equal(loader.load().success, true);
  const found: string[] = [];
  for (const d of loader.getProject().getPreEmitDiagnostics()) {
    const file = d.getSourceFile()?.getFilePath();
    if (!file || file.includes(`${path.sep}node_modules${path.sep}`) || !file.startsWith(repo + path.sep)) continue;
    found.push(`${path.relative(repo, file)}:${d.getLineNumber()}:TS${d.getCode()}`);
  }
  return found.sort();
}

