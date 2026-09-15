/**
 * carrick#1162: a consumer request sent as `body: JSON.stringify(payload)` was
 * captured as `string` and judged against the producer's object contract.
 *
 * The v1 inferrer already reads through `JSON.stringify` to the payload. When
 * its text is not usable (a member it could only type as `any`), the alias
 * falls to its capture infer anchor, whose raw locator matched the
 * `JSON.stringify(...)` call and printed the call's own `string` result. A
 * serialised body is the JSON of its argument, so the capture reads the
 * argument, on one line or several.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';

const CLIENT_TS = `export interface CreateGroup {
  workspaceId: string;
  documentIds: string[];
}

declare function send(init: { method: string; body: string }): Promise<unknown>;

export function createGroup(input: CreateGroup) {
  return send({
    method: "POST",
    body: JSON.stringify(input),
  });
}

export function createLink(workspaceId: string, billId: string) {
  return send({
    method: "POST",
    body: JSON.stringify({
      workspaceId,
      billId,
    }),
  });
}
`;

describe('capture reads a serialised request body through JSON.stringify (#1162)', () => {
  let repoDir: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1162-capture-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler' },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'client.ts'), CLIENT_TS);
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  function lineOf(text: string): number {
    const at = CLIENT_TS.indexOf(text);
    assert.ok(at >= 0, `fixture must contain: ${text}`);
    return CLIENT_TS.slice(0, at).split('\n').length;
  }

  it('prints the payload, not the string the call returns', () => {
    const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1162-out-'));
    const result = captureStub({
      repoRoot: repoDir,
      serviceName: 'serialised-body',
      outDir,
      anchors: [
        {
          kind: 'infer',
          alias: 'Endpoint_group_Request',
          source_file: 'src/client.ts',
          anchor_origin: 'deterministic-infer',
          line_number: lineOf('body: JSON.stringify(input)'),
          expression_text: 'JSON.stringify(input)',
        },
        {
          kind: 'infer',
          alias: 'Endpoint_link_Request',
          source_file: 'src/client.ts',
          anchor_origin: 'deterministic-infer',
          line_number: lineOf('body: JSON.stringify({'),
          expression_text: 'JSON.stringify({\n      workspaceId,\n      billId,\n    })',
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    const surface = fs.readFileSync(path.join(outDir, 'types/surface.d.ts'), 'utf-8');
    /** The whole declaration of `alias`, whitespace collapsed (the printer wraps objects). */
    const declared = (alias: string): string => {
      const start = surface.indexOf(`export type ${alias} =`);
      assert.ok(start >= 0, `surface must declare ${alias}:\n${surface}`);
      const next = surface.indexOf('export type ', start + 1);
      return surface
        .slice(start, next < 0 ? undefined : next)
        .replace(/\s+/g, ' ')
        .trim();
    };
    for (const alias of ['Endpoint_group_Request', 'Endpoint_link_Request']) {
      const line = declared(alias);
      assert.ok(!/=\s*string;/.test(line), `a serialised body is not a string contract: ${line}`);
    }
    const group = declared('Endpoint_group_Request');
    assert.ok(/documentIds/.test(group) || /CreateGroup/.test(group), group);
    assert.match(declared('Endpoint_link_Request'), /billId: string/);
  });
});
