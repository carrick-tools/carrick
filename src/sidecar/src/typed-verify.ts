/**
 * Verify mode (carrick#2886): `CARRICK_TYPED_VERIFY=1` makes the daemon answer
 * every request twice, warm from what it holds and cold in a fresh process
 * given the same `init` and request, and compare the two.
 *
 * It is for CI and for proofs, never on by default: the cold answer costs what
 * a cold scan costs. When the answers differ the difference is logged and the
 * cold answer is the one returned, so a verified daemon can only be as wrong as
 * a cold process.
 *
 * Union member order is the one difference a held program can show without
 * being wrong (the compiler lists members in the order it met them, #735), so
 * the comparison is made twice: byte for byte over the answer with its timing
 * taken out, then over the same text with every union put in canonical order.
 * An answer that differs only by that order is counted apart and says so in
 * the log; it is still replaced by the cold one.
 */

import { spawn } from 'node:child_process';
import { canonicalizeUnionsInText } from './type-text-canonicalizer.js';

/** Whether verify mode is on for this process. */
export function verifyEnabled(env: NodeJS.ProcessEnv = process.env): boolean {
  return env.CARRICK_TYPED_VERIFY === '1';
}

/** What verify mode has seen so far, as `status` reports it. */
export interface VerifyCounts {
  /** Requests answered both ways. */
  checked: number;
  /** Answers that differ in more than union member order. */
  mismatches: number;
  /** Answers that differ only in union member order. */
  order_only: number;
}

export function newVerifyCounts(): VerifyCounts {
  return { checked: 0, mismatches: 0, order_only: 0 };
}

export type Comparison = 'equal' | 'order_only' | 'different';

/** The frame with its timing fields (`*_ms`, `infer_timing`) taken out, at any depth. */
function untimed(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(untimed);
  if (value !== null && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [key, inner] of Object.entries(value as Record<string, unknown>)) {
      if (key === 'infer_timing' || key.endsWith('_ms')) continue;
      out[key] = untimed(inner);
    }
    return out;
  }
  return value;
}

/** Every string in `value` with its unions in canonical order. */
function canonicalUnions(value: unknown): unknown {
  if (typeof value === 'string') return canonicalizeUnionsInText(value);
  if (Array.isArray(value)) return value.map(canonicalUnions);
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>).map(([k, v]) => [k, canonicalUnions(v)]));
  }
  return value;
}

/** Compare two terminal frames. */
export function compareFrames(warm: string, cold: string): Comparison {
  const parse = (frame: string): unknown => untimed(JSON.parse(frame));
  const a = parse(warm);
  const b = parse(cold);
  if (JSON.stringify(a) === JSON.stringify(b)) return 'equal';
  return JSON.stringify(canonicalUnions(a)) === JSON.stringify(canonicalUnions(b)) ? 'order_only' : 'different';
}

/** The first position at which two answers differ, for the log. */
export function firstDifference(warm: string, cold: string): string {
  const a = JSON.stringify(untimed(JSON.parse(warm)));
  const b = JSON.stringify(untimed(JSON.parse(cold)));
  let at = 0;
  while (at < a.length && at < b.length && a[at] === b[at]) at += 1;
  const from = Math.max(0, at - 40);
  return `at ${at}: warm ...${a.slice(from, at + 80)} / cold ...${b.slice(from, at + 80)}`;
}

/** Answers `line` (after `init`, when given) in a fresh stdio sidecar; resolves to its terminal frame. */
export type ColdRunner = (init: object | null, line: string) => Promise<string>;

/** The cold runner: a fresh process of the sidecar that is running, over stdio. */
export function spawnColdRunner(entry: string, timeoutMs = 600_000): ColdRunner {
  return (init, line) =>
    new Promise((resolve, reject) => {
      const env = { ...process.env };
      delete env.CARRICK_TYPED_VERIFY;
      const child = spawn(process.execPath, [entry], { stdio: ['pipe', 'pipe', 'ignore'], env });
      const wanted = JSON.parse(line).request_id as string;
      let buffer = '';
      const timer = setTimeout(() => finish(new Error(`cold process gave no answer in ${timeoutMs} ms`)), timeoutMs);
      const finish = (result: string | Error): void => {
        clearTimeout(timer);
        child.kill('SIGKILL');
        if (result instanceof Error) reject(result);
        else resolve(result);
      };
      child.on('error', (err) => finish(err));
      child.on('exit', () => finish(new Error('cold process ended before answering')));
      child.stdout.on('data', (chunk: Buffer) => {
        buffer += chunk.toString();
        const lines = buffer.split('\n');
        buffer = lines.pop() ?? '';
        for (const frame of lines) {
          if (!frame.trim()) continue;
          const parsed = JSON.parse(frame) as { request_id?: string; status?: string };
          if (parsed.status === 'progress' || parsed.request_id !== wanted) continue;
          finish(frame);
          return;
        }
      });
      if (init) child.stdin.write(JSON.stringify(init) + '\n');
      child.stdin.write(line + '\n');
    });
}

export interface VerifyOutcome {
  /** The terminal frame to return: the warm one when they agree, else the cold one. */
  frame: string;
  comparison: Comparison;
}

/**
 * Check a warm terminal frame against a cold answer to the same request. The
 * frame returned is the warm one when they agree and the cold one otherwise;
 * a cold process that fails leaves the warm frame, logged.
 */
export async function verifyAnswer(
  warm: string,
  init: object | null,
  line: string,
  cold: ColdRunner,
  counts: VerifyCounts,
  log: (message: string) => void
): Promise<VerifyOutcome> {
  const what = `${(JSON.parse(line) as { action?: string }).action ?? 'request'} ${(JSON.parse(line) as { request_id?: string }).request_id ?? ''}`.trim();
  let coldFrame: string;
  try {
    coldFrame = await cold(init, line);
  } catch (err) {
    log(`verify: ${what}: no cold answer to compare (${err instanceof Error ? err.message : String(err)})`);
    return { frame: warm, comparison: 'equal' };
  }
  counts.checked += 1;
  const comparison = compareFrames(warm, coldFrame);
  if (comparison === 'order_only') {
    counts.order_only += 1;
    log(`verify: ${what}: warm and cold differ only in union member order (returned the cold answer)`);
  } else if (comparison === 'different') {
    counts.mismatches += 1;
    log(`verify: ${what}: warm and cold answers differ, returned the cold answer; ${firstDifference(warm, coldFrame)}`);
  }
  return { frame: comparison === 'equal' ? warm : coldFrame, comparison };
}
