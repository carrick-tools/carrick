/**
 * Reply-status T2a: a route whose handler is declared on the row's line, with
 * a locator naming one of its error sends. Where every reply the handler
 * returns states an error or redirect status, or a success status with no
 * body, the route has no success payload: the answer is `unknown` with the
 * decided reason `no_success_payload`. A blind answer here (no entry, or
 * `any` with `no_payload_evidence`) lets the status-blind capture re-read the
 * row and store the error body as the route's whole response type.
 *
 * Both kinds of checkout are covered: `tiny-reply` is installed (its sends
 * type as the platform `Response`), `absent-kit` is not (every send it makes
 * resolves to nothing, as in a scan with no dependencies installed). No
 * framework name is read.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const KIT_DTS = `export declare function json(body: unknown, init?: { status?: number }): Response;
export declare function redirect(url: string, status?: number): Response;
`;

/** One body per kit: the import line names the kit. */
const handlers = (kit: string): string => `import { json, redirect } from '${kit}';

declare function run(id: string): { match<A, B>(ok: () => A, err: () => B): Promise<A | B> };
declare function load(id: string): any;
declare function clip<T>(rows: T[], max: number): T[];
declare function next(): Response;

export async function allErrors(request: Request, id: string) {
  if (request.method !== 'POST') {
    return json({ error: 'Method Not Allowed' }, { status: 405 });
  }
  if (!id) {
    return json({ error: 'Invalid params' }, { status: 400 });
  }
  return await run(id).match(
    () => new Response(null, { status: 204 }),
    () => json({ error: 'gone' }, { status: 404 })
  );
}

export async function errorPlusSuccess(request: Request, id: string) {
  if (!id) {
    return json({ error: 'Invalid params' }, { status: 400 });
  }
  return json({ items: [id] }, { status: 200 });
}

export async function anySuccess(request: Request, id: string) {
  if (!id) {
    return json({ error: 'Invalid params' }, { status: 400 });
  }
  return json(load(id));
}

export async function redirectOnly(request: Request, id: string) {
  if (!id) {
    return redirect('/login', 302);
  }
  return redirect('/home', 303);
}

export async function noBody(request: Request, id: string) {
  if (!id) {
    return new Response(null, { status: 204 });
  }
  return new Response(undefined, { status: 202 });
}

export async function unstatedEmpty(request: Request, id: string) {
  if (!id) {
    return json({ error: 'Invalid params' }, { status: 400 });
  }
  return new Response(null);
}

export async function handsOn(request: Request, id: string) {
  if (!id) {
    return json({ error: 'Invalid params' }, { status: 400 });
  }
  return next();
}

export async function clipControl(rows: string[], id: string) {
  if (!id) {
    return json({ error: 'Invalid params' }, { status: 400 });
  }
  return clip(rows, 404);
}
`;

/** The reply forms with a context or reply parameter, from a kit that is not installed. */
const BARE_FORMS = `import type { Ctx, Reply } from 'absent-kit';

export async function ctxGone(c: Ctx, id: string) {
  if (!id) {
    return c.json({ error: 'missing' }, 400);
  }
  return c.json({ error: 'gone' }, 410);
}

export async function chainGone(reply: Reply, id: string) {
  if (!id) {
    return reply.status(400).send({ error: 'missing' });
  }
  return reply.status(410).send({ error: 'gone' });
}
`;

interface Entry {
  alias: string;
  type_string: string;
  any_provenance?: Array<{ reason: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();
const reasons = (got?: Entry): string[] => (got?.any_provenance ?? []).map((p) => p.reason);

describe('reply-status T2a: a declared handler with no success body decides no_success_payload', () => {
  let client: SidecarClient;
  let repoDir: string;
  const sources = new Map<string, string>();

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-t2a-'));
    const kitDir = path.join(repoDir, 'node_modules', 'tiny-reply');
    fs.mkdirSync(kitDir, { recursive: true });
    fs.writeFileSync(
      path.join(kitDir, 'package.json'),
      JSON.stringify({ name: 'tiny-reply', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(kitDir, 'index.d.ts'), KIT_DTS);
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
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    sources.set('typed.ts', handlers('tiny-reply'));
    sources.set('bare.ts', handlers('absent-kit'));
    sources.set('forms.ts', BARE_FORMS);
    for (const [file, text] of sources) {
      fs.writeFileSync(path.join(repoDir, 'src', file), text);
    }

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The row a scan sends: the handler's line, the locator on an error send. */
  async function response(file: string, handler: string, locator: string): Promise<Entry | undefined> {
    const lines = (sources.get(file) ?? '').split('\n');
    const line = lines.findIndex((l) => l.includes(`function ${handler}(`)) + 1;
    assert.ok(line > 0, `${file} declares no ${handler}`);
    const at = lines.findIndex((l, i) => i >= line && l.includes(locator)) + 1;
    assert.ok(at > 0, `${handler} holds no ${locator}`);
    const alias = `Endpoint_${file.replace('.ts', '')}_${handler}_Response_At1`;
    const res = await client.send<{ inferred_types?: Entry[] }>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: path.join(repoDir, 'src', file),
          line_number: line,
          infer_kind: 'response_body',
          expression_text: locator,
          expression_line: at,
          alias,
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  const decided = (got: Entry | undefined, what: string): void => {
    assert.ok(got, `${what}: an entry is published`);
    assert.strictEqual(collapse(got.type_string), 'unknown', `${what}: ${JSON.stringify(got)}`);
    assert.deepStrictEqual(reasons(got), ['no_success_payload'], what);
  };

  for (const file of ['typed.ts', 'bare.ts']) {
    it(`${file}: error replies beside a body-less success decide no_success_payload`, async () => {
      decided(await response(file, 'allErrors', "{ error: 'Method Not Allowed' }"), file);
    });

    it(`${file}: a handler that only redirects decides no_success_payload`, async () => {
      decided(await response(file, 'redirectOnly', "'/login'"), file);
    });

    it(`${file}: a handler whose replies carry no body decides no_success_payload`, async () => {
      decided(await response(file, 'noBody', 'new Response(null, { status: 204 })'), file);
    });

    it(`${file}: a success with no stated status keeps the row undecided`, async () => {
      const got = await response(file, 'unstatedEmpty', "{ error: 'Invalid params' }");
      assert.ok(!reasons(got).includes('no_success_payload'), JSON.stringify(got));
    });

    it(`${file}: a returned call with no argument keeps the row undecided`, async () => {
      const got = await response(file, 'handsOn', "{ error: 'Invalid params' }");
      assert.ok(!reasons(got).includes('no_success_payload'), JSON.stringify(got));
    });

    it(`${file}: a success body read as any keeps the row undecided`, async () => {
      const got = await response(file, 'anySuccess', "{ error: 'Invalid params' }");
      assert.ok(!reasons(got).includes('no_success_payload'), JSON.stringify(got));
    });
  }

  it('typed.ts: an error beside a success body still reads the success body', async () => {
    const got = await response('typed.ts', 'errorPlusSuccess', "{ error: 'Invalid params' }");
    assert.ok(got);
    assert.strictEqual(collapse(got.type_string), '{ items: string[]; }');
  });

  it('bare.ts: an error beside a success body keeps its unresolved-callee answer', async () => {
    const got = await response('bare.ts', 'errorPlusSuccess', "{ error: 'Invalid params' }");
    assert.ok(got);
    assert.strictEqual(collapse(got.type_string), 'any');
    assert.deepStrictEqual(reasons(got), ['no_payload_evidence']);
  });

  // The installed-kit twin of this control decides through the machinery
  // path's own all-dropped test, which reads `clip(rows, 404)` as an error
  // reply; that test is not this exit's (filed beside this ticket).
  it('bare.ts: a returned call that is no send keeps the row undecided', async () => {
    const got = await response('bare.ts', 'clipControl', "{ error: 'Invalid params' }");
    assert.ok(got);
    assert.strictEqual(collapse(got.type_string), 'any');
    assert.deepStrictEqual(reasons(got), ['no_payload_evidence']);
  });

  it('a context send and a reply chain that state only errors decide no_success_payload', async () => {
    decided(await response('forms.ts', 'ctxGone', "{ error: 'missing' }"), 'ctxGone');
    decided(await response('forms.ts', 'chainGone', "{ error: 'missing' }"), 'chainGone');
  });
});
