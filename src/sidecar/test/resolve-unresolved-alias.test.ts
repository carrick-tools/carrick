/**
 * carrick#1444 — an alias whose own type the stub project could not resolve is
 * not an answer, whatever the compiler prints for it.
 *
 * `resolve_definitions` runs over the emitted declaration tree alone, with no
 * dependencies installed (the stub is written to a temp dir, so nothing
 * resolves out of it). A reference that leaves the tree therefore lands on
 * TypeScript's unresolved-reference placeholder — `TypeFlags.Any` carrying the
 * internal `intrinsicName === 'error'` — and the compiler prints that
 * placeholder as the REFERENCE TEXT it failed to resolve, not as `any`. An
 * instantiation over a dependency's internal generics reads as a confident
 * name with no members in it, and the publication rule that refuses an empty
 * answer (`text_is_bare_top_type` on the scanner side) never fires, so the
 * operation is counted typed and the index hands a reader a name that resolves
 * nowhere.
 *
 * At a MEMBER position the same echo is worth keeping — `status:
 * EngagementStatus` names something in the producer's own repo — so this is
 * scoped to the alias's own type, which is the whole answer for that
 * operation.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

interface ResolveResponseShape {
  request_id: string;
  status: string;
  definitions?: Array<{ type_alias: string; definition: string; expanded: string }>;
  errors?: string[];
}

const UNRESOLVED = 'Endpoint_unresolved_Response';
const CLEAN = 'Endpoint_clean_Response';
const MEMBER = 'Endpoint_member_Response';

/**
 * A stub tree whose surface names three aliases in one module:
 *  - one instantiating a generic imported from a package that is not present,
 *  - one plain interface declared in the tree,
 *  - one interface with a MEMBER typed by a name from the absent package.
 */
function writeStub(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-unresolved-'));
  const types = path.join(dir, 'types');
  fs.mkdirSync(types, { recursive: true });
  fs.writeFileSync(
    path.join(types, 'model.d.ts'),
    [
      "import type { Projection, Marker } from 'a-package-that-is-not-installed';",
      'declare const selection: {',
      '    include: {',
      '        parts: true;',
      '    };',
      '};',
      'export type ProjectedRow = Projection<typeof selection>;',
      'export interface PlainRow {',
      '    id: string;',
      '    label: string;',
      '}',
      'export interface RowWithMarker {',
      '    id: string;',
      '    marker: Marker;',
      '}',
      '',
    ].join('\n'),
  );
  fs.writeFileSync(
    path.join(types, 'surface.d.ts'),
    [
      `export type ${UNRESOLVED} = import('./model').ProjectedRow;`,
      `export type ${CLEAN} = import('./model').PlainRow;`,
      `export type ${MEMBER} = import('./model').RowWithMarker;`,
      '',
    ].join('\n'),
  );
  return dir;
}

describe('an unresolvable alias is not a definition (carrick#1444)', () => {
  let client: SidecarClient;
  let stubDir: string;
  let definitions: Map<string, { definition: string; expanded: string }>;

  before(async () => {
    stubDir = writeStub();
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'unres-init', repo_root: stubDir });
    const resolved = await client.send<ResolveResponseShape>({
      action: 'resolve_definitions',
      request_id: 'unres-resolve',
      stub_dir: stubDir,
      aliases: [UNRESOLVED, CLEAN, MEMBER],
    });
    assert.strictEqual(
      resolved.status,
      'success',
      `resolve failed: ${JSON.stringify(resolved.errors)}`,
    );
    definitions = new Map(
      (resolved.definitions ?? []).map((d) => [
        d.type_alias,
        { definition: d.definition, expanded: d.expanded },
      ]),
    );
  });

  after(async () => {
    await client?.stop();
    fs.rmSync(stubDir, { recursive: true, force: true });
  });

  it('answers the top type for an alias whose type did not resolve, not the reference text', () => {
    const def = definitions.get(UNRESOLVED);
    assert.ok(def, `expected an entry for ${UNRESOLVED}`);
    assert.strictEqual(
      def.expanded.trim(),
      'any',
      `an alias the stub could not resolve must answer the top type, got: ${def.expanded}`,
    );
    assert.strictEqual(
      def.definition.trim(),
      'any',
      `the as-written form must not echo the reference either, got: ${def.definition}`,
    );
  });

  it('still inlines an alias the tree does declare', () => {
    const def = definitions.get(CLEAN);
    assert.ok(def, `expected an entry for ${CLEAN}`);
    assert.match(def.expanded, /id:\s*string/);
    assert.match(def.expanded, /label:\s*string/);
  });

  it('keeps the name a MEMBER is typed by, which is the producer’s own vocabulary', () => {
    const def = definitions.get(MEMBER);
    assert.ok(def, `expected an entry for ${MEMBER}`);
    assert.match(def.expanded, /id:\s*string/);
    assert.match(
      def.expanded,
      /marker:\s*Marker/,
      `a member's unresolved name stays as written, got: ${def.expanded}`,
    );
  });
});
