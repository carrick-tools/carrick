/**
 * carrick#4212: a handler that reads its body through a whole-object cast,
 * `(request as unknown as { body: Order }).body`, reads the `body` member of
 * the cast's object. The route's request body is that member's type, not the
 * cast's whole object (one extra level, a false mismatch for every caller).
 *
 * The read is followed by the member access written after the cast; no cast
 * shape is matched.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ROUTES_TS = `interface Order { id: string; qty: number }
interface Incoming { body: unknown }
declare const app: {
  post(path: string, handler: (request: Incoming) => unknown): void;
};

app.post('/member-of-cast', (request) => {
  const order = (request as unknown as { body: Order }).body;
  return order;
});

app.post('/whole-cast', (request) => {
  const whole = request as unknown as { body: Order };
  return whole;
});
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

function registration(route: string): { start: number; end: number; line: number } {
  const start = ROUTES_TS.indexOf(`app.post('${route}'`);
  assert.ok(start >= 0, `fixture must register ${route}`);
  let depth = 0;
  for (let i = start; i < ROUTES_TS.length; i += 1) {
    if (ROUTES_TS[i] === '(') depth += 1;
    if (ROUTES_TS[i] === ')') {
      depth -= 1;
      if (depth === 0) {
        return { start, end: i + 1, line: ROUTES_TS.slice(0, start).split('\n').length };
      }
    }
  }
  throw new Error(`unbalanced registration for ${route}`);
}

describe('carrick#4212: a member read past a cast publishes the member', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4212-'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', skipLibCheck: true },
        include: ['src'],
      })
    );
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

  const requestBody = async (route: string) => {
    const span = registration(route);
    const alias = `Request_${route.replace(/\W/g, '_')}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        { file_path: routesPath, line_number: span.line, span_start: span.start, span_end: span.end, infer_kind: 'request_body', alias },
      ],
    });
    return response.inferred_types?.find((t) => t.alias === alias)?.type_string.replace(/\s+/g, ' ');
  };

  it('publishes the member the handler reads, not the cast object', async () => {
    const text = await requestBody('/member-of-cast');
    assert.ok(text, 'an answer is published');
    assert.ok(!/body\s*:/.test(text), `must not carry the extra body level: ${text}`);
    assert.match(text, /id: string/);
    assert.match(text, /qty: number/);
  });

  it('keeps the whole cast where nothing is read off it', async () => {
    const text = await requestBody('/whole-cast');
    assert.match(text ?? '', /body\s*:/);
  });
});
