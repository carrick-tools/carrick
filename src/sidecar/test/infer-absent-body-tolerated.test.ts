/**
 * carrick#2814: the inferrer marks a route's request body, or a subscriber's
 * payload, whose handler works when nothing is sent (`absent_body_tolerated`).
 *
 * A server library hands a handler `undefined` where a request carried no
 * body. A handler that defaults the body, or reads it only through `?.` or
 * `?? {}`, survives that; one that destructures it or reads a member off it
 * throws, so it requires a body whatever its type says. A handler handed on
 * whole, or declared where no body of it is in reach, answers nothing and is
 * not marked.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

/** A made-up server and event library: a request generic in its body, and a bus. */
const LIBRARY_DTS = `export interface Req<B> {
  body: B;
  query: Record<string, string>;
}
export interface App {
  post<B>(path: string, ...handlers: Array<(request: Req<B>) => unknown>): void;
}
export declare const app: App;
export interface Bus {
  on<P>(topic: string, handler: (payload: P) => void): void;
}
export declare const bus: Bus;
export interface JobQuery {
  limit?: number;
  label?: string;
}
export declare function outOfReach(request: Req<JobQuery>): unknown;
`;

const SERVICE_TS = `import { app, bus, outOfReach, type Req, type JobQuery } from "jobs-server";

declare function audit(request: Req<JobQuery>): void;

app.post("/jobs/nullish", async (request: Req<JobQuery>) => {
  const { limit } = request.body ?? {};
  return { limit };
});

app.post("/jobs/optional-chain", async (request: Req<JobQuery>) => {
  return { limit: request.body?.limit, label: request.body?.label };
});

app.post("/jobs/default", async ({ body = {} }: Req<JobQuery>) => {
  return { limit: body.limit };
});

app.post("/jobs/alias", async (request: Req<JobQuery>) => {
  const input = request.body;
  return { limit: input?.limit };
});

app.post("/jobs/destructure", async (request: Req<JobQuery>) => {
  const { limit } = request.body;
  return { limit };
});

app.post("/jobs/member", async (request: Req<JobQuery>) => {
  return { limit: request.body.limit };
});

app.post("/jobs/handed-on", async (request: Req<JobQuery>) => {
  audit(request);
  return { limit: request.body?.limit };
});

app.post("/jobs/out-of-reach", outOfReach);

app.post("/jobs/middleware", (request: Req<JobQuery>) => audit(request), async (request: Req<JobQuery>) => {
  return { limit: request.body?.limit };
});

bus.on("jobs.tolerant", (event: JobQuery) => {
  console.log(event?.limit);
});

bus.on("jobs.destructured", ({ limit }: JobQuery) => {
  console.log(limit);
});
`;

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    absent_body_tolerated?: boolean;
  }>;
}

/** The 1-based line of the one source line containing `marker`. */
function lineOf(marker: string): number {
  const lines = SERVICE_TS.split('\n');
  const hits = lines.flatMap((line, index) => (line.includes(marker) ? [index + 1] : []));
  assert.strictEqual(hits.length, 1, `marker ${marker} must name one line`);
  return hits[0];
}

describe('carrick#2814: a handler that tolerates an absent body is marked', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2814-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const libDir = path.join(repoDir, 'node_modules', 'jobs-server');
    fs.mkdirSync(libDir, { recursive: true });
    fs.writeFileSync(
      path.join(libDir, 'package.json'),
      JSON.stringify({ name: 'jobs-server', version: '1.0.0', types: './index.d.ts' })
    );
    fs.writeFileSync(path.join(libDir, 'index.d.ts'), LIBRARY_DTS);
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    servicePath = path.join(repoDir, 'src', 'service.ts');
    fs.writeFileSync(servicePath, SERVICE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** A line-only request locator at the route's registration, as a scan sends it. */
  async function inferRoute(marker: string) {
    const alias = `Endpoint_${marker.replace(/\W+/g, '_')}_Request`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [{ file_path: servicePath, line_number: lineOf(marker), infer_kind: 'request_body', alias }],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  async function inferPayload(marker: string, paramName: string) {
    const alias = `Endpoint_${marker.replace(/\W+/g, '_')}_Payload`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: servicePath,
          line_number: lineOf(marker),
          infer_kind: 'function_param',
          param_name: paramName,
          alias,
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  for (const route of ['"/jobs/nullish"', '"/jobs/optional-chain"', '"/jobs/default"', '"/jobs/alias"']) {
    it(`marks a handler that tolerates no body: ${route}`, async () => {
      const inferred = await inferRoute(route);
      assert.ok(inferred, 'the row must be answered');
      assert.match(inferred.type_string, /JobQuery|limit\?: number/);
      assert.strictEqual(inferred.absent_body_tolerated, true);
    });
  }

  for (const route of [
    '"/jobs/destructure"',
    '"/jobs/member"',
    '"/jobs/handed-on"',
    '"/jobs/out-of-reach"',
    '"/jobs/middleware"',
  ]) {
    it(`does not mark a handler that dereferences its body or is out of reach: ${route}`, async () => {
      const inferred = await inferRoute(route);
      assert.ok(inferred, 'the row must be answered');
      assert.match(inferred.type_string, /JobQuery|limit\?: number/);
      assert.strictEqual(inferred.absent_body_tolerated, undefined);
    });
  }

  it('marks a subscriber that reads its payload through `?.`', async () => {
    const inferred = await inferPayload('"jobs.tolerant"', 'event');
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(inferred.absent_body_tolerated, true);
  });

  it('does not mark a subscriber that destructures its payload', async () => {
    const inferred = await inferPayload('"jobs.destructured"', '{ limit }');
    assert.ok(inferred, 'the row must be answered');
    assert.strictEqual(inferred.absent_body_tolerated, undefined);
  });
});
