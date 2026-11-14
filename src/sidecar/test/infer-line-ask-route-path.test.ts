/**
 * carrick#4381: a line-only ask on a line that registers two routes read the
 * first registration's handler, so the second route's row answered the first
 * route's body. The scanner sends the row's own path (`route_path`, the full
 * path after any mounts) on such a line; the registration whose own path the
 * row's path ends with is the one the ask is for.
 *
 * An ask with no `route_path`, or a path no registration on the line states
 * (a computed path), keeps today's reading: the first registration.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const TYPES_TS = `export interface OwnRequest { body: unknown }
export interface OwnReply { send(value?: unknown): OwnReply; code(status: number): OwnReply }
export interface OwnServer { post(path: string, handler: (request: OwnRequest, reply: OwnReply) => unknown): OwnServer }
`;

const ROUTES_TS = `import type { OwnServer, OwnRequest, OwnReply } from './types';
interface Item { id: string }
declare const item: Item;
declare const dynamicPath: string;

async function sendItems(q: OwnRequest, reply: OwnReply) {
  return reply.send([item]);
}
async function statusOnly(q: OwnRequest, reply: OwnReply) {
  return reply.code(204);
}

export function routes(own: OwnServer) {
  own.post('/pick-send', sendItems).post('/pick-code', statusOnly);
  own.post('/first-code', statusOnly).post('/last-send', sendItems);
  own.post('/dyn-first', statusOnly).post(dynamicPath, sendItems);
}
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

describe('carrick#4381: a line-only ask picks the registration its route path names', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4381-'));
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
    fs.writeFileSync(path.join(repoDir, 'package.json'), JSON.stringify({ name: 'fixture-4381' }));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(path.join(repoDir, 'src', 'types.ts'), TYPES_TS);
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

  const lineOf = (route: string) =>
    ROUTES_TS.slice(0, ROUTES_TS.indexOf(`'${route}'`)).split('\n').length;

  const ask = async (route: string, routePath: string | undefined, line = lineOf(route)) => {
    const alias = `Line_${route.replace(/\W/g, '_')}_${routePath ? 'p' : 'n'}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: routesPath,
          infer_kind: 'response_body',
          alias,
          line_number: line,
          ...(routePath ? { route_path: routePath } : {}),
        },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias);
  };

  for (const route of ['/pick-send', '/last-send']) {
    it(`${route}: the send route reads its array whichever registration is first`, async () => {
      const answer = await ask(route, route);
      assert.ok(answer, `${route} must answer`);
      assert.match(answer.type_string, /id: string/);
    });
  }

  for (const route of ['/pick-code', '/first-code']) {
    it(`${route}: the status-only route answers nothing`, async () => {
      const answer = await ask(route, route);
      assert.strictEqual(answer, undefined, `${route}: got ${JSON.stringify(answer)}`);
    });
  }

  it('/last-send: a path with a mount prefix still names its registration', async () => {
    const answer = await ask('/last-send', '/api/v1/last-send');
    assert.ok(answer, 'must answer');
    assert.match(answer.type_string, /id: string/);
  });

  it('/first-code: a mount prefix on the inner route still names its registration', async () => {
    const answer = await ask('/first-code', '/api/v1/first-code');
    assert.strictEqual(answer, undefined, `got ${JSON.stringify(answer)}`);
  });

  it('/pick-send: no route_path keeps the first registration', async () => {
    // The first registration on the line starts with the outer call, which
    // registers `/pick-code`: status only, no answer.
    const answer = await ask('/pick-send', undefined);
    assert.strictEqual(answer, undefined, `got ${JSON.stringify(answer)}`);
  });

  it('/dyn-first: a computed path on the other registration does not hide the stated one', async () => {
    const answer = await ask('/dyn-first', '/dyn-first');
    assert.strictEqual(answer, undefined, `got ${JSON.stringify(answer)}`);
  });

  it('a path no registration on the line states keeps the first registration', async () => {
    const answer = await ask('/dyn-first', '/not-on-this-line');
    assert.ok(answer, 'the outer registration (computed path) answers');
    assert.match(answer.type_string, /id: string/);
  });
});
