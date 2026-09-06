/**
 * carrick#695: classify a bare `x.verb("/lit", arg)` site by its RECEIVER.
 *
 * A member call whose first argument is a route-shaped literal carries no role
 * of its own: `app.get("/widgets", handler)` registers a route and
 * `api.get("/widgets", options)` requests one, and nothing in the call's shape
 * separates them (a last-argument function, an options bag and a verb name can
 * each equally belong to either). The ruling of 2026-09-05 is that no
 * structural rule may decide it.
 *
 * What CAN decide it is the receiver's type, which is a compiler question. This
 * kind answers exactly that and nothing more: what `x` is at the site, which
 * package DECLARES that type, and what the invoked member returns. It never
 * says "server" or "client" — the classification from those facts to a role
 * lives in the Rust driver, where the detected framework and data-fetcher
 * package lists are.
 *
 * The fixture installs two declaration-only packages, because that is the
 * condition the answer depends on: with no declarations on disk the receiver's
 * type is `any` and this kind must say so rather than guess.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const APP_TS = `import { createServer } from "server-fw";
import { createClient } from "http-fetcher";
import { createThing } from "absent-package";

interface Widget {
  id: string;
}

const app = createServer();
const api = createClient();
const unresolved = createThing();

const local = {
  get(path: string, options: unknown): void {},
};

app.get("/widgets", (req, res) => {});
api.get("/widgets", { retries: 2 });
local.get("/widgets", {});
unresolved.get("/widgets", {});
`;

const SERVER_DTS = `export declare class Server {
  get(path: string, handler: (req: unknown, res: unknown) => void): void;
  post(path: string, handler: (req: unknown, res: unknown) => void): void;
}
export declare function createServer(): Server;
`;

const CLIENT_DTS = `export declare class HttpClient {
  get<T = unknown>(path: string, options?: { retries?: number }): Promise<T>;
}
export declare function createClient(): HttpClient;
`;

function spanOf(marker: string): { start: number; end: number; line: number } {
  const start = APP_TS.indexOf(marker);
  assert.ok(start >= 0, `fixture must contain: ${marker}`);
  const end = start + marker.length;
  const line = APP_TS.slice(0, start).split('\n').length;
  return { start, end, line };
}

const APP_CALL = spanOf('app.get("/widgets", (req, res) => {})');
const API_CALL = spanOf('api.get("/widgets", { retries: 2 })');
const LOCAL_CALL = spanOf('local.get("/widgets", {})');
const UNRESOLVED_CALL = spanOf('unresolved.get("/widgets", {})');

interface InferShape {
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    declaring_package?: string;
    member_return_type?: string;
  }>;
  errors?: string[];
}

function writePackage(root: string, name: string, dts: string): void {
  const dir = path.join(root, 'node_modules', name);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(
    path.join(dir, 'package.json'),
    JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' })
  );
  fs.writeFileSync(path.join(dir, 'index.d.ts'), dts);
}

describe('carrick#695 receiver_type inference', () => {
  let repoDir: string;
  let client: SidecarClient;
  let appPath: string;
  let byAlias: Map<string, { type_string: string; declaring_package?: string; member_return_type?: string }>;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-695-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          rootDir: 'src',
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    appPath = path.join(repoDir, 'src', 'app.ts');
    fs.writeFileSync(appPath, APP_TS);
    writePackage(repoDir, 'server-fw', SERVER_DTS);
    writePackage(repoDir, 'http-fetcher', CLIENT_DTS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });

    const response = (await client.send({
      action: 'infer',
      request_id: 'receivers',
      requests: [
        { file_path: appPath, line_number: APP_CALL.line, span_start: APP_CALL.start, span_end: APP_CALL.end, infer_kind: 'receiver_type', alias: 'app_site' },
        { file_path: appPath, line_number: API_CALL.line, span_start: API_CALL.start, span_end: API_CALL.end, infer_kind: 'receiver_type', alias: 'api_site' },
        { file_path: appPath, line_number: LOCAL_CALL.line, span_start: LOCAL_CALL.start, span_end: LOCAL_CALL.end, infer_kind: 'receiver_type', alias: 'local_site' },
        { file_path: appPath, line_number: UNRESOLVED_CALL.line, span_start: UNRESOLVED_CALL.start, span_end: UNRESOLVED_CALL.end, infer_kind: 'receiver_type', alias: 'unresolved_site' },
      ],
    })) as InferShape;

    assert.equal(response.status, 'success', JSON.stringify(response.errors));
    byAlias = new Map(
      (response.inferred_types ?? []).map((entry) => [entry.alias, entry])
    );
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  it('names the package that declares a receiver from a dependency', () => {
    const app = byAlias.get('app_site');
    assert.ok(app, 'the server receiver resolved');
    assert.equal(app.type_string, 'Server');
    assert.equal(app.declaring_package, 'server-fw');

    const api = byAlias.get('api_site');
    assert.ok(api, 'the client receiver resolved');
    assert.equal(api.type_string, 'HttpClient');
    assert.equal(api.declaring_package, 'http-fetcher');
  });

  it('reports the invoked member return type, awaited', () => {
    assert.equal(byAlias.get('app_site')?.member_return_type, 'void');
    assert.equal(byAlias.get('api_site')?.member_return_type, 'unknown');
  });

  it('names no package for a receiver the workspace itself declares', () => {
    const local = byAlias.get('local_site');
    assert.ok(local, 'the workspace-declared receiver resolved');
    assert.equal(local.declaring_package, undefined);
  });

  it('says `any` rather than guessing when the receiver is unresolvable', () => {
    const unresolved = byAlias.get('unresolved_site');
    assert.ok(unresolved, 'the unresolved receiver still produced an answer');
    assert.equal(unresolved.type_string, 'any');
    assert.equal(unresolved.declaring_package, undefined);
  });
});
