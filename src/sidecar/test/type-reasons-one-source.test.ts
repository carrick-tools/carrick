/**
 * `src/capture/type-reasons.json` is the one list of type reasons (carrick#4231): every
 * word a top-type finding can carry, the sentence a reader is shown for it, and
 * when it means the scan DECIDED there is no contract. The engine and the Rust
 * driver both read it; neither keeps a list of its own. Reads source text for
 * the Rust side; a read or parse miss throws, so a rename cannot pass on empty
 * sets.
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
const LIST = path.join(SIDECAR, 'src', 'capture', 'type-reasons.json');
const RUST = path.join(SIDECAR, '..', 'engine', 'type_compat_v2.rs');
const API = path.join(SIDECAR, 'src', 'capture', 'api.ts');

type Entry = { sentence: string; decided: 'always' | 'at_call_result' | 'never' };

function list(): Record<string, Entry> {
  const parsed = JSON.parse(fs.readFileSync(LIST, 'utf8')) as Record<string, Entry>;
  if (Object.keys(parsed).length === 0) throw new Error('parse miss: type-reasons.json is empty');
  return parsed;
}

describe('type-reasons.json is the one list', () => {
  it('holds every word, each with a sentence and one of three decided values', () => {
    const entries = list();
    assert.equal(Object.keys(entries).length, 19);
    for (const [word, entry] of Object.entries(entries)) {
      assert.match(word, /^[a-z_]+$/, word);
      assert.ok(entry.sentence.length > 0, `${word} has no sentence`);
      assert.ok(['always', 'at_call_result', 'never'].includes(entry.decided), `${word}: ${entry.decided}`);
    }
  });

  it("the engine's decided set is the list's `always` words", () => {
    const always = Object.entries(list())
      .filter(([, e]) => e.decided === 'always')
      .map(([w]) => w)
      .sort();
    assert.ok(always.length > 0);
    assert.deepEqual([...DECIDED_ABSTAIN_REASONS].sort(), always);
  });

  it('the Rust driver reads the list and keeps none of its own', () => {
    const src = fs.readFileSync(RUST, 'utf8');
    assert.ok(src.includes('include_str!("../sidecar/src/capture/type-reasons.json")'), 'Rust does not include the list');
    assert.doesNotMatch(src, /const DECIDED_ABSTAIN_REASONS: &\[&str\]/);
  });

  it('TypeProvenanceReason is the list\'s keys, not a hand-kept union', () => {
    const src = fs.readFileSync(API, 'utf8');
    const m = /export type TypeProvenanceReason =([^;]*);/.exec(src);
    if (!m) throw new Error('parse miss: TypeProvenanceReason not found in capture/api.ts');
    assert.match(m[1], /keyof typeof/);
    assert.doesNotMatch(m[1], /'[a-z_]+'/);
  });
});
