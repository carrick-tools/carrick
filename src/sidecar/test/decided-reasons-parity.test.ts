/**
 * The engine's decided-reason list and the Rust driver's list must agree
 * (carrick#4231). Both say which `unknown` answers are a decision, not a
 * failure to read. Parses the Rust slice and the `TypeProvenanceReason` union
 * from source text; a parse miss throws, so a rename cannot pass on empty sets.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { DECIDED_ABSTAIN_REASONS } from '../src/type-inferrer.js';

const here = path.dirname(fileURLToPath(import.meta.url));
// Compiled to <sidecar>/dist/test; the sources sit beside dist.
const SIDECAR = path.join(here, '..', '..');
const RUST = path.join(SIDECAR, '..', 'engine', 'type_compat_v2.rs');
const API = path.join(SIDECAR, 'src', 'capture', 'api.ts');

function words(body: string, re: RegExp): string[] {
  return [...body.matchAll(re)].map((m) => m[1]);
}

function rustDecided(): Set<string> {
  const src = fs.readFileSync(RUST, 'utf8');
  const m = /const DECIDED_ABSTAIN_REASONS: &\[&str\] = &\[([\s\S]*?)\];/.exec(src);
  if (!m) throw new Error('parse miss: DECIDED_ABSTAIN_REASONS not found in type_compat_v2.rs');
  const w = words(m[1], /"([a-z_]+)"/g);
  if (w.length === 0) throw new Error('parse miss: Rust decided list is empty');
  return new Set(w);
}

function provenanceUnion(): Set<string> {
  const src = fs.readFileSync(API, 'utf8');
  const m = /export type TypeProvenanceReason =([\s\S]*?);/.exec(src);
  if (!m) throw new Error('parse miss: TypeProvenanceReason not found in capture/api.ts');
  const w = words(m[1], /'([a-z_]+)'/g);
  if (w.length === 0) throw new Error('parse miss: TypeProvenanceReason is empty');
  return new Set(w);
}

describe('decided-reason lists', () => {
  it('engine set equals the Rust list', () => {
    const rust = rustDecided();
    const engine = new Set(DECIDED_ABSTAIN_REASONS);
    const missingInEngine = [...rust].filter((w) => !engine.has(w)).sort();
    const missingInRust = [...engine].filter((w) => !rust.has(w)).sort();
    assert.deepEqual(
      { missingInEngine, missingInRust },
      { missingInEngine: [], missingInRust: [] },
      `engine lacks ${missingInEngine.join(',') || '-'}; Rust lacks ${missingInRust.join(',') || '-'}`,
    );
  });

  it('both lists are subsets of TypeProvenanceReason', () => {
    const union = provenanceUnion();
    const unknownWords = [...rustDecided(), ...DECIDED_ABSTAIN_REASONS].filter(
      (w) => !union.has(w),
    );
    assert.deepEqual(unknownWords, []);
  });
});
