/**
 * carrick#2005: a file-based route's response row is located at a send's own
 * line, and a send's line is not a route registration. The rule that types a
 * route from every success send of the handler the located send sits in
 * (carrick#1161) must run there too, and must read a status an earlier call
 * of the send's chain states (`reply.status(401).json(body)`).
 *
 * Every name below is a synthetic stand-in. `reply-kit` carries the transport
 * surface a Node response object has; nothing consults its name.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const REPLY_DTS = `export interface Reply {
  statusCode: number;
  setHeader(name: string, value: string): void;
  getHeader(name: string): string | undefined;
  status(code: number): Reply;
  json(body: unknown): Reply;
  end(text?: string): Reply;
}
export interface Ask { query: Record<string, string | undefined>; }
`;

const ROUTES_TS = `import type { Ask, Reply } from "reply-kit";

interface Made { id: string; label: string }
declare const session: string | null;
declare const wanted: number;
declare function make(): Promise<Made>;
declare function openItems(): Promise<string[]>;

export async function guarded(req: Ask, res: Reply) {
  if (!session) return res.status(401).json({ error: "Unauthorized" });
  const made = await make();
  return res.status(201).json(made);
}

export async function modes(req: Ask, res: Reply) {
  const open = await openItems();
  if (open.length === 0) return res.json({ ready: false, total: 0 });
  if (req.query.view === 'split') {
    return res.json({ ready: true, open: open.length, closed: 0 });
  }
  return res.json({ ready: true, total: open.length });
}

export async function unreturned(req: Ask, res: Reply) {
  if (!session) {
    res.status(403).json({ error: "Forbidden" });
    return;
  }
  res.json({ done: true });
}

export async function single(req: Ask, res: Reply) {
  return res.json({ only: 1 });
}

export async function dynamic(req: Ask, res: Reply) {
  if (!session) return res.status(wanted).json({ problem: "upstream" });
  return res.json({ fine: true });
}

export async function allErrors(req: Ask, res: Reply) {
  if (!session) return res.status(401).json({ error: "Denied" });
  return res.status(500).json({ error: "broken" });
}
`;

function lineOf(text: string): number {
  const at = ROUTES_TS.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return ROUTES_TS.slice(0, at).split('\n').length;
}

interface InferShape {
  status: string;
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#2005 a response row located at a send inside its handler', () => {
  let repoDir: string;
  let routesPath: string;
  let client: SidecarClient;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2005-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.mkdirSync(path.join(repoDir, 'node_modules', 'reply-kit'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'node_modules', 'reply-kit', 'package.json'),
      JSON.stringify({ name: 'reply-kit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(repoDir, 'node_modules', 'reply-kit', 'index.d.ts'), REPLY_DTS);
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

  /** The row a file-based route gets: the send's own line and its body. */
  async function inferAtSend(alias: string, sendText: string, bodyText: string) {
    const line = lineOf(sendText);
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: routesPath,
          line_number: line,
          infer_kind: 'response_body',
          alias,
          expression_text: bodyText,
          expression_line: line,
        },
      ],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  it('publishes the success body when the row is the guard whose chain states 401', async () => {
    const inferred = await inferAtSend(
      'Guarded',
      'res.status(401).json({ error: "Unauthorized" })',
      '{ error: "Unauthorized" }'
    );
    assert.ok(inferred);
    const text = collapse(inferred.type_string);
    assert.ok(!/error/.test(text), `an error body is not the contract, got: ${text}`);
    assert.ok(/label: string/.test(text) && /id: string/.test(text), `got: ${text}`);
  });

  it('joins the success sends when the located send is itself a success send', async () => {
    const inferred = await inferAtSend(
      'Modes',
      'res.json({ ready: false, total: 0 })',
      '{ ready: false, total: 0 }'
    );
    assert.ok(inferred);
    const text = collapse(inferred.type_string);
    assert.ok(/closed/.test(text), `a sibling mode's body joins the route's, got: ${text}`);
    assert.ok(/total/.test(text), `got: ${text}`);
  });

  it('publishes a success send the handler does not return when the row names it', async () => {
    const inferred = await inferAtSend('Unreturned', 'res.json({ done: true })', '{ done: true }');
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ done: boolean; }');
  });

  it('leaves a handler with one send exactly as the located send reads', async () => {
    const inferred = await inferAtSend('Single', 'res.json({ only: 1 })', '{ only: 1 }');
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), '{ only: number; }');
  });

  it('keeps a send whose chain status is not a literal, as an argument one is', async () => {
    const inferred = await inferAtSend(
      'Dynamic',
      'res.status(wanted).json({ problem: "upstream" })',
      '{ problem: "upstream" }'
    );
    assert.ok(inferred);
    const text = collapse(inferred.type_string);
    assert.ok(/problem/.test(text) && /fine/.test(text), `got: ${text}`);
  });

  it('abstains when every send of the handler states an error status', async () => {
    const inferred = await inferAtSend(
      'AllErrors',
      'res.status(401).json({ error: "Denied" })',
      '{ error: "Denied" }'
    );
    assert.ok(inferred);
    assert.strictEqual(collapse(inferred.type_string), 'unknown');
  });
});
