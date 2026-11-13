/**
 * Package tools the scan spawns get only the environment they need: no
 * Carrick credential and no CI runner token, while the user's own registry
 * auth and the tool's cache and path settings pass through.
 */

import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { DenoProject, findDenoConfig, runCheck } from '../src/capture/index.js';
import { toolEnv } from '../src/capture/tool-env.js';
import type { CheckPairSpec } from '../src/capture/api.js';

const hasDeno = spawnSync('deno', ['--version']).status === 0;

/** Runs `fn` with `vars` set in process.env, then restores the old values. */
async function withEnv<T>(vars: Record<string, string>, fn: () => T | Promise<T>): Promise<T> {
  const saved = Object.fromEntries(Object.keys(vars).map(k => [k, process.env[k]]));
  Object.assign(process.env, vars);
  try {
    return await fn();
  } finally {
    for (const [k, v] of Object.entries(saved)) {
      if (v === undefined) delete process.env[k];
      else process.env[k] = v;
    }
  }
}

/**
 * A registry stand-in on loopback, in its own process: the Deno spawn is
 * synchronous, so a server on this process's event loop could not answer.
 * It logs each request's Authorization header and answers 404.
 */
async function startRegistry(log: string): Promise<{ port: number; child: ChildProcess }> {
  const script = `
    const http = require('node:http'); const fs = require('node:fs');
    const server = http.createServer((req, res) => {
      fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({ url: req.url, authorization: req.headers.authorization ?? null }) + '\\n');
      res.statusCode = 404; res.end();
    });
    server.listen(0, '127.0.0.1', () => process.stdout.write(String(server.address().port) + '\\n'));
  `;
  const child = spawn(process.execPath, ['-e', script], { stdio: ['ignore', 'pipe', 'inherit'] });
  const port = await new Promise<number>((resolve, reject) => {
    child.once('error', reject);
    child.stdout!.once('data', chunk => resolve(Number(String(chunk).trim())));
  });
  return { port, child };
}

function readLog(log: string): Array<{ url: string; authorization: string | null }> {
  if (!fs.existsSync(log)) return [];
  return fs.readFileSync(log, 'utf8').split('\n').filter(Boolean).map(line => JSON.parse(line));
}

/** A Deno project with one npm import, whose .npmrc names the given variable as the registry token. */
function denoFixture(root: string, port: number, tokenVar: string): void {
  fs.mkdirSync(path.join(root, 'src'), { recursive: true });
  fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({ tasks: {} }));
  fs.writeFileSync(path.join(root, 'deno.lock'), JSON.stringify({
    version: '5',
    specifiers: { 'npm:probe-pkg@1': '1.0.0' },
    npm: { 'probe-pkg@1.0.0': { integrity: 'sha512-AAAA' } },
  }));
  fs.writeFileSync(path.join(root, 'src', 'app.ts'), 'import { x } from "npm:probe-pkg@1";\nexport const y = x;\n');
  fs.writeFileSync(path.join(root, '.npmrc'),
    `registry=http://127.0.0.1:${port}/\n//127.0.0.1:${port}/:_authToken=\${${tokenVar}}\n`);
}

/** Loads the fixture through DenoProject on a cold cache and returns what the registry saw. */
async function denoRequests(tokenVar: string, vars: Record<string, string>) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-tool-env-deno-'));
  const log = path.join(root, 'requests.jsonl');
  const { port, child } = await startRegistry(log);
  try {
    const project = path.join(root, 'project');
    denoFixture(project, port, tokenVar);
    await withEnv({ ...vars, DENO_DIR: path.join(root, 'deno-dir') }, () => {
      // The stand-in has no package, so loading may fail; only the requests matter.
      try { new DenoProject(findDenoConfig(project)!, project); } catch { /* expected */ }
    });
    return readLog(log);
  } finally {
    child.kill();
    fs.rmSync(root, { recursive: true, force: true });
  }
}

describe('toolEnv', () => {
  it('drops Carrick, CI-runner and cloud credentials and keeps everything else', () => {
    const source = {
      PATH: '/bin', HOME: '/home/u', DENO_DIR: '/cache', HTTPS_PROXY: 'http://proxy', NPM_TOKEN: 'npm', NODE_AUTH_TOKEN: 'node',
      CI: 'true', GITHUB_ACTIONS: 'true',
      CARRICK_TOKEN: 'x', CARRICK_API_KEY: 'x', GITHUB_TOKEN: 'x', GH_TOKEN: 'x',
      ACTIONS_RUNTIME_TOKEN: 'x', ACTIONS_ID_TOKEN_REQUEST_TOKEN: 'x', ACTIONS_ID_TOKEN_REQUEST_URL: 'x',
      AWS_ACCESS_KEY_ID: 'x', AWS_SECRET_ACCESS_KEY: 'x', AWS_SESSION_TOKEN: 'x',
    };
    assert.deepEqual(toolEnv({ DENO_NO_UPDATE_CHECK: '1' }, source), {
      PATH: '/bin', HOME: '/home/u', DENO_DIR: '/cache', HTTPS_PROXY: 'http://proxy', NPM_TOKEN: 'npm', NODE_AUTH_TOKEN: 'node',
      CI: 'true', GITHUB_ACTIONS: 'true', DENO_NO_UPDATE_CHECK: '1',
    });
  });
});

describe('the check install gets the tool environment', () => {
  it('passes PATH and no Carrick credential to the package manager', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-tool-env-check-'));
    try {
      for (const name of ['orders', 'web']) {
        const dir = path.join(root, name);
        fs.mkdirSync(path.join(dir, 'types'), { recursive: true });
        fs.writeFileSync(path.join(dir, 'package.json'),
          JSON.stringify({ name: `@carrick/${name}`, version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' }));
        fs.writeFileSync(path.join(dir, 'types', 'surface.d.ts'), 'export type T = { a: string; };\n');
      }
      const dump = path.join(root, 'env.txt');
      const pnpmPath = path.join(root, 'pnpm-env');
      fs.writeFileSync(pnpmPath, `#!/bin/sh\nenv > ${JSON.stringify(dump)}\nexit 1\n`);
      fs.chmodSync(pnpmPath, 0o755);
      const pairs: CheckPairSpec[] = [{
        pair_key: 'pair-a', protocol: 'http', type_kind: 'response',
        producer: { service_name: 'orders', alias: 'T' }, consumer: { service_name: 'web', alias: 'T' },
      }];
      await withEnv({ CARRICK_TOKEN: 'sentinel-check', GITHUB_TOKEN: 'sentinel-gh' }, () => runCheck({
        stubs: [
          { service_name: 'orders', stub_dir: path.join(root, 'orders') },
          { service_name: 'web', stub_dir: path.join(root, 'web') },
        ],
        pairs, pnpmPath,
      }));
      const env = fs.readFileSync(dump, 'utf8');
      assert.match(env, /^PATH=/m);
      assert.doesNotMatch(env, /sentinel-check|sentinel-gh/);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});

describe('Deno registry requests', { skip: !hasDeno }, () => {
  it('carry no Carrick credential or CI token named by the project .npmrc', async () => {
    const requests = await denoRequests('CARRICK_TOKEN', { CARRICK_TOKEN: 'sentinel-carrick', GITHUB_TOKEN: 'sentinel-github' });
    assert.ok(requests.length >= 1, 'the registry saw no request, so the test proves nothing');
    for (const r of requests) {
      assert.doesNotMatch(r.authorization ?? '', /sentinel-carrick|sentinel-github/, JSON.stringify(r));
    }
  });

  it('still carry the registry token the user configured', async () => {
    const requests = await denoRequests('NPM_TOKEN', { NPM_TOKEN: 'kept' });
    assert.ok(requests.length >= 1, 'the registry saw no request, so the test proves nothing');
    assert.ok(requests.some(r => r.authorization === 'Bearer kept'), JSON.stringify(requests));
  });
});
