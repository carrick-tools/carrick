/**
 * carrick#4379: a call handed a function was read as a route registration, so
 * a located `items.map((item) => item.code(200))` answered what its callback
 * returns, not the array it produces. A registration hands its function to a
 * server or router; a call on an array hands it to an iteration, and a call
 * on a promise hands it to a continuation. Neither is a registration, whatever
 * its callback returns.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SRC = `interface Coded { label: string; code(status: number): Coded }
interface OwnReply { send(value?: unknown): OwnReply }
interface OwnServer { post(path: string, handler: (reply: OwnReply) => unknown): OwnServer }
declare const items: Coded[];
declare const pending: Promise<Coded>;
declare const own: OwnServer;
interface Item { id: string }
declare const item: Item;

export const coded = items.map((entry) => entry.code(200));
export const lengths = items.map((entry) => entry.label.length);
export const settled = pending.then((entry) => entry.label);
export const kept = items.filter((entry) => entry.label.length > 0);
own.post('/real', (reply) => reply.send([item]));
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

describe('carrick#4379: a registration needs a registrar, not an iteration or a continuation', () => {
  let client: SidecarClient;
  let repoDir: string;
  let file: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4379-'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'package.json'), JSON.stringify({ name: 'fixture-4379' }));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    file = path.join(repoDir, 'src', 'main.ts');
    fs.writeFileSync(file, SRC);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const lineOf = (index: number) => SRC.slice(0, index).split('\n').length;

  /** The call that starts at `startText`, asked by its span (as the scanner sends a located call). */
  const bySpan = async (startText: string) => {
    const start = SRC.indexOf(startText);
    let depth = 0;
    let end = start;
    for (let i = SRC.indexOf('(', start); i < SRC.length; i++) {
      if (SRC[i] === '(') depth++;
      if (SRC[i] === ')' && --depth === 0) {
        end = i + 1;
        break;
      }
    }
    const alias = `Span_${startText.replace(/\W/g, '_')}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: file,
          infer_kind: 'response_body',
          alias,
          line_number: lineOf(start),
          span_start: start,
          span_end: end,
        },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  // The callback's return is not the call's value: these answered `number`,
  // `boolean` and `string`. A located call that is not a registration is read
  // as any other call is, and a primitive-element array or a promise of a
  // primitive is no payload the located call states, so it abstains.
  for (const [label, startText] of [
    ['items.map', 'items.map((entry) => entry.label.length'],
    ['items.filter', 'items.filter('],
    ['pending.then', 'pending.then('],
  ]) {
    it(`${label}: the located call never answers its callback's return`, async () => {
      const answer = await bySpan(startText);
      assert.strictEqual(answer, undefined, `got ${JSON.stringify(answer?.type_string)}`);
    });
  }

  it('own.post: a registration still answers what its handler sends', async () => {
    const answer = await bySpan("own.post('/real'");
    assert.ok(answer, 'must answer');
    assert.match(answer.type_string, /id: string/);
  });

  // carrick#4341 named this move: a call inside an array's callback that is
  // handed only a status answered nothing, because the callback was read as a
  // registration's handler. It is read as the callback it is again.
  it('items.map: a status-shaped call inside the callback answers its own type', async () => {
    const text = 'entry.code(200)';
    const at = SRC.indexOf(text);
    const alias = 'InCallback';
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: file,
          infer_kind: 'response_body',
          alias,
          line_number: lineOf(at),
          span_start: at,
          span_end: at + text.length,
        },
      ],
    });
    const answer = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(answer, 'must answer');
    assert.match(answer.type_string, /label: string/);
  });
});
