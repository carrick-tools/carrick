/**
 * carrick#4909 T1: the types of a procedure named by the variable that
 * holds it (contract amendment 13, `name: { bound: "binding" }`).
 *
 * The producer row sits on the line its maker call starts on. Its request is
 * what a caller sends: the member at the label's `payload` key of parameter 0
 * of the variable's own call signature (the validator's input, not its
 * output). Its response is that signature's awaited return.
 *
 * The fixture is `tests/fixtures/rpc-binding/` (service `declared`), with
 * invented packages; its README is the answer key.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const FIXTURE = path.join(
  __dirname,
  '..',
  '..',
  '..',
  '..',
  'tests',
  'fixtures',
  'rpc-binding',
  'repo',
  'services',
  'declared'
);

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#4909: a binding-named procedure is typed from its own call signature', () => {
  let client: SidecarClient;
  let repoDir: string;
  let notesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4909-'));
    fs.cpSync(FIXTURE, repoDir, { recursive: true });
    notesPath = path.join(repoDir, 'src', 'notes.functions.ts');
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function infer(
    line: number,
    kind: 'procedure_input' | 'procedure_output',
    member?: string
  ) {
    const alias = `Endpoint_L${line}_${kind}`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: notesPath,
          line_number: line,
          infer_kind: kind,
          alias,
          ...(member ? { member } : {}),
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('reads saveNote\'s request at the payload key: the validator\'s input', async () => {
    const inferred = await infer(5, 'procedure_input', 'data');
    assert.ok(inferred, 'the request must be answered');
    assert.strictEqual(collapse(inferred.type_string), '{ body: string; }');
  });

  it('reads saveNote\'s response as the awaited return', async () => {
    const inferred = await infer(5, 'procedure_output');
    assert.ok(inferred, 'the response must be answered');
    assert.strictEqual(collapse(inferred.type_string), '{ ok: boolean; }');
  });

  it('reads listNotes\'s request as undefined: it takes no input', async () => {
    const inferred = await infer(3, 'procedure_input', 'data');
    assert.ok(inferred, 'the request must be answered');
    assert.strictEqual(collapse(inferred.type_string), 'undefined');
  });

  it('reads listNotes\'s response as an array', async () => {
    const inferred = await infer(3, 'procedure_output');
    assert.ok(inferred, 'the response must be answered');
    assert.strictEqual(collapse(inferred.type_string), '{ id: number; }[]');
  });

  it('answers nothing at a payload key the signature does not have', async () => {
    const inferred = await infer(5, 'procedure_input', 'body');
    assert.strictEqual(inferred, undefined);
  });

  it('answers nothing on a line that declares no procedure variable', async () => {
    const inferred = await infer(1, 'procedure_output');
    assert.strictEqual(inferred, undefined);
  });
});
