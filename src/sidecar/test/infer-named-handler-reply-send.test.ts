/**
 * carrick#4309: a send-span ask inside a NAMED handler published a
 * repository-declared reply.
 *
 * `own.post('/n', handler)` with `async function handler(q, reply) { return
 * reply.send([item]); }`: the send-span ask on `reply.send([item])` answered
 * the reply object, while the line ask on the registration read the sent
 * value (carrick#4283). The ask's line is the send's line, and the handler's
 * parent is not a call, so neither way #4283 finds a registration's handler
 * reached it. The handler is found through its registration: a reference to
 * its name handed to a call that registers it.
 *
 * A function only ever called directly, and a named callback handed to
 * `.map` that returns a typed setter, keep their answers.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ROUTES_TS = `interface Item { id: string }
declare const item: Item;

interface OwnRequest { body: unknown }
interface OwnReply { send(value?: unknown): OwnReply; code(status: number): OwnReply }
interface OwnServer { post(path: string, handler: (request: OwnRequest, reply: OwnReply) => unknown): OwnServer }

export function ownRoutes(own: OwnServer) {
  own.post('/declared', declaredHandler);
  own.post('/bound', boundHandler);
}

async function declaredHandler(_request: OwnRequest, reply: OwnReply) {
  return reply.send([item]);
}

const boundHandler = async (_request: OwnRequest, reply: OwnReply) => {
  return reply.send([item]);
};

interface Journal { lines: string[]; write(entry: unknown): Journal }
function direct(journal: Journal): Journal {
  return journal.write('x');
}
declare const journal: Journal;
export const written = direct(journal);

interface Builder { name: string; set(value: string): Builder }
function rename(builder: Builder): Builder {
  return builder.set('x');
}
declare const builders: Builder[];
export const renamed = builders.map(rename);
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

/** The span of the first `text` after `anchor`, as the scanner sends it. */
function spanOf(text: string, anchor: string): { start: number; end: number; line: number } {
  const from = ROUTES_TS.indexOf(anchor);
  assert.ok(from >= 0, `fixture must hold ${anchor}`);
  const start = ROUTES_TS.indexOf(text, from);
  assert.ok(start >= 0, `fixture must hold ${text} after ${anchor}`);
  return { start, end: start + text.length, line: ROUTES_TS.slice(0, start).split('\n').length };
}

describe('carrick#4309: a send-span ask inside a named handler reads the value it sends', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4309-'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022' },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'package.json'), JSON.stringify({ name: 'fixture-4309' }));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, ROUTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const ask = async (alias: string, located: Record<string, number>) => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [{ file_path: routesPath, ...located, infer_kind: 'response_body', alias }],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  const bySend = (alias: string, text: string, anchor: string) => {
    const span = spanOf(text, anchor);
    return ask(alias, { line_number: span.line, span_start: span.start, span_end: span.end });
  };

  for (const [route, anchor] of [
    ['/declared', 'async function declaredHandler'],
    ['/bound', 'const boundHandler'],
  ] as const) {
    it(`${route}: a send-span ask in the named handler reads the value it sends`, async () => {
      const answer = await bySend(`Send_${route.slice(1)}`, 'reply.send([item])', anchor);
      assert.ok(answer, `${route} must answer`);
      assert.doesNotMatch(answer.type_string, /send/);
      assert.match(answer.type_string, /id: string/);
    });

    it(`${route}: a line ask on the named handler's own line reads the value it sends`, async () => {
      const line = ROUTES_TS.slice(0, ROUTES_TS.indexOf(anchor)).split('\n').length;
      const answer = await ask(`Line_${route.slice(1)}`, { line_number: line });
      assert.ok(answer, `${route} must answer`);
      assert.doesNotMatch(answer.type_string, /send/);
      assert.match(answer.type_string, /id: string/);
    });
  }

  it('a function only ever called directly keeps its answer', async () => {
    const answer = await bySend('Direct', "journal.write('x')", 'function direct');
    assert.ok(answer, 'the call must answer');
    assert.match(answer.type_string, /lines: string\[\]/);
  });

  it('a named callback handed to .map that returns a typed setter keeps its answer', async () => {
    const answer = await bySend('Rename', "builder.set('x')", 'function rename');
    assert.ok(answer, 'the call must answer');
    assert.match(answer.type_string, /name: string/);
  });
});
