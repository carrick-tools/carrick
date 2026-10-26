/**
 * carrick#2824: a located response expression that sits in another route's
 * handler is not the row's. The row's registration sits on its own line; a
 * locator pointing into the next registration's handler used to publish that
 * route's reply. The reader drops such a locator and answers by the line.
 *
 * A locator inside the row's own registration or handler, and one outside any
 * registration, is read as before.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SERVICE_TS = `type TrendA = { kind: 'a'; points: number[] };
type TrendB = { kind: 'b'; label: string };
type TrendC = { kind: 'c'; total: number };

type Handler = (req: { id: string }) => unknown;
declare const app: {
  get(path: string, handler: Handler): void;
  get(path: string, mw: Handler, handler: Handler): void;
};
declare function register(a: typeof app, path: string, handler: Handler): void;
function importedHandler(req: { id: string }) {
  const out = { imported: true as const };
  return out;
}
declare const mw: Handler;

export function reportRoutes() {
  app.get('/a', async (req) => {
    const response: TrendA = { kind: 'a', points: [1] };
    return response;
  });
  register(app, '/b', async (req) => {
    const response: TrendB = { kind: 'b', label: 'x' };
    return response;
  });
  app.get('/c', async (req) => {
    const response: TrendC = { kind: 'c', total: 3 };
    return response;
  });
}

export function importedRoutes() {
  app.get('/x', mw, importedHandler);
  app.get('/y', async (req) => {
    const inlineOnly = { inline: 'y' as const };
    return inlineOnly;
  });
}

export function middlewareRoutes() {
  app.get('/m1', mw, async (req) => {
    const response = { m: 1 as const };
    return response;
  });
}
`;

const lineOf = (text: string, nth = 0): number => {
  let at = -1;
  for (let i = 0; i <= nth; i++) {
    at = SERVICE_TS.indexOf(text, at + 1);
    assert.ok(at >= 0, `fixture must contain: ${text}`);
  }
  return SERVICE_TS.slice(0, at).split('\n').length;
};

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#2824: a locator in another route\'s handler is dropped', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;
  let n = 0;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2824-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['es2022'] },
        include: ['src'],
      })
    );
    servicePath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(servicePath, SERVICE_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-2824', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (kind: string, rowLine: number, text: string | undefined, textLine: number | undefined): Promise<string | undefined> => {
    const alias = `T${n++}`;
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: servicePath,
          line_number: rowLine,
          expression_text: text,
          expression_line: textLine,
          infer_kind: kind,
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    return inferred ? collapse(inferred.type_string) : undefined;
  };

  for (const kind of ['function_return', 'response_body']) {
    it(`${kind}: a locator in the third handler answers the first row's own type`, async () => {
      const text = await infer(kind, lineOf("app.get('/a'"), 'response', lineOf('return response;', 2));
      assert.ok(text && /kind: "a"|kind: 'a'/.test(text) && !/'c'|"c"/.test(text), `expected TrendA, got: ${text}`);
    });

    it(`${kind}: a locator in the row's own handler still answers it`, async () => {
      const text = await infer(kind, lineOf("app.get('/a'"), 'response', lineOf('return response;', 0));
      assert.ok(text && /kind: "a"|kind: 'a'/.test(text), `expected TrendA, got: ${text}`);
    });

    it(`${kind}: a locator in a handler a local helper registers is foreign too`, async () => {
      const text = await infer(kind, lineOf("app.get('/c'"), 'response', lineOf('return response;', 1));
      assert.ok(text && /kind: "c"|kind: 'c'/.test(text), `expected TrendC, got: ${text}`);
    });

    it(`${kind}: an imported handler row ignores a locator in an inline route`, async () => {
      const line = lineOf("app.get('/x'");
      const withLocator = await infer(kind, line, 'inlineOnly', lineOf('return inlineOnly;'));
      const lineOnly = await infer(kind, line, undefined, undefined);
      assert.strictEqual(withLocator, lineOnly);
      assert.ok(!/inline/.test(withLocator ?? ''), `inline route leaked: ${withLocator}`);
      if (kind === 'response_body') {
        assert.ok(/imported/.test(withLocator ?? ''), `expected the imported handler's reply, got: ${withLocator}`);
      }
    });

    it(`${kind}: a middleware registration reads a locator inside it as before`, async () => {
      const text = await infer(kind, lineOf("app.get('/m1'"), 'response', lineOf('return response;', 3));
      assert.ok(text && /m: 1/.test(text), `expected the middleware route's body, got: ${text}`);
    });
  }
});
