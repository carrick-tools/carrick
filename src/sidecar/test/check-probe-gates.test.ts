/**
 * The probe's gate aliases, compiled by the real compiler (in memory).
 *
 * The classifier tests feed synthetic diagnostics, so they cannot see which
 * gates a given type actually trips. This file swaps the probe's two surface
 * imports for local aliases and reads the TS2344 gate lines tsc reports, so a
 * gate that also fires on a top type (as `[any] extends [void]` does) fails
 * here rather than silently changing the reported reason.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import ts from 'typescript';
import { buildProbe } from '../src/capture/check-probe.js';
import type { CheckPairSpec } from '../src/capture/api.js';

const PKG = (s: string) => `@carrick/${s}`;

function requestSpec(): CheckPairSpec {
  return {
    pair_key: 'gates',
    protocol: 'http',
    type_kind: 'request',
    producer: { service_name: 'orders', alias: 'Endpoint_a_Request' },
    consumer: { service_name: 'web', alias: 'Call_a_Request' },
  };
}

/** Gate names tsc trips when the sent side is `sentType`. */
function firedGates(sentType: string): string[] {
  const plan = buildProbe(requestSpec(), PKG);
  const lines = plan.source.split('\n');
  const [sentImport, expectedImport] = plan.importLines;
  lines[sentImport - 1] = `type Sent = ${sentType};`;
  lines[expectedImport - 1] = `type Expected = { title: string };`;
  const fileName = '/probe/pair.ts';
  const text = lines.join('\n');
  const options: ts.CompilerOptions = {
    strict: true,
    noEmit: true,
    target: ts.ScriptTarget.ES2022,
    lib: ['lib.es2022.d.ts', 'lib.dom.d.ts'],
    types: [],
  };
  const host = ts.createCompilerHost(options);
  const readFile = host.getSourceFile.bind(host);
  host.getSourceFile = (name, version, onError, create) =>
    name === fileName
      ? ts.createSourceFile(name, text, version)
      : readFile(name, version, onError, create);
  const program = ts.createProgram([fileName], options, host);
  const source = program.getSourceFile(fileName)!;
  return ts
    .getPreEmitDiagnostics(program, source)
    .filter((d) => d.code === 2344 && d.file?.fileName === fileName)
    .map((d) => source.getLineAndCharacterOfPosition(d.start!).line + 1)
    .map((line): string | undefined => plan.gateLines.get(line))
    .filter((name): name is string => name !== undefined && name.startsWith('sent:'))
    .sort();
}

describe('probe gates under the real compiler (carrick#1162)', () => {
  const cases: Array<[string, string[]]> = [
    ['any', ['sent:any']],
    ['unknown', ['sent:unknown']],
    ['never', ['sent:never']],
    ['void', ['sent:void']],
    ['undefined', ['sent:void']],
    ['FormData', ['sent:form']],
    ['URLSearchParams', ['sent:form']],
    ['{ title: string }', []],
  ];
  for (const [sent, expected] of cases) {
    it(`a sent side of \`${sent}\` trips exactly ${JSON.stringify(expected)}`, () => {
      assert.deepStrictEqual(firedGates(sent), expected);
    });
  }
});
