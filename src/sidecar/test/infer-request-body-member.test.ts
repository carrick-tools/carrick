/**
 * carrick#2057: a request located at a member of the body its handler parses.
 *
 * A handler reads its body off the platform request once and binds it
 * (`const body = (await request.json()) as ImportBody`). A locator that names
 * a member of that binding (`body.items`), or a name destructured out of the
 * read, names ONE field of the body. Publishing that field's type states the
 * whole body is a list of entries, and every caller that sends the object
 * reads incompatible.
 *
 * The body is what the source states at the read: a cast on it, or the
 * annotation of the binding it initialises. Where the source states nothing,
 * nothing is published. A locator that names the binding itself, or a member
 * of anything that is not the parsed body, keeps its reading.
 *
 * The platform types are TypeScript's own `dom` library; no framework or
 * method name is matched.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const ROUTES_TS = `type Entry = { name: string; note?: string | null };
type ImportBody = { items: Entry[]; dryRun: boolean };
type Upstream = { rate: number };

export async function castMember(request: Request) {
  const body = (await request.json()) as ImportBody;
  const items = body.items;
  return Response.json({ imported: items.length });
}

export async function annotatedMember(request: Request) {
  const payload: ImportBody = await request.json();
  return Response.json({ imported: payload.items.length, dry: payload.dryRun });
}

export async function destructured(request: Request) {
  const { items: entries } = (await request.json()) as ImportBody;
  return Response.json({ imported: entries.length });
}

export async function untypedMember(request: Request) {
  const raw = await request.json();
  const rows = raw.items;
  return Response.json({ imported: rows.length });
}

export async function wholeBinding(request: Request) {
  const whole = (await request.json()) as ImportBody;
  return Response.json({ imported: whole.items.length });
}

export async function parameterMember(request: Request, context: { params: { id: string } }) {
  const ident = context.params;
  return Response.json({ id: ident.id, url: request.url });
}

export async function outboundMember(request: Request) {
  const upstream = (await (await fetch(request.url)).json()) as Upstream;
  const rate = upstream.rate;
  return Response.json({ rate });
}
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

function lineOf(marker: string): number {
  const index = ROUTES_TS.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
  return index + 1;
}

const IMPORT_BODY = '{ items: { name: string; note?: null | string; }[]; dryRun: boolean; }';

describe('carrick#2057: a request located at a member of the parsed body', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routesPath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2057-'));
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

  const infer = async (alias: string, expression: string, at: string): Promise<string | undefined> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: routesPath,
          line_number: lineOf(at),
          expression_text: expression,
          expression_line: lineOf(at),
          infer_kind: 'request_body',
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    return inferred ? collapse(inferred.type_string) : undefined;
  };

  it('publishes the body a cast states, not the member the locator names', async () => {
    assert.strictEqual(await infer('CastMember', 'body.items', 'const items = body.items;'), IMPORT_BODY);
  });

  it('publishes the body an annotation states, for a member read inside an expression', async () => {
    assert.strictEqual(
      await infer('AnnotatedMember', 'payload.items', 'imported: payload.items.length'),
      IMPORT_BODY
    );
  });

  it('publishes the body for a name destructured out of the read', async () => {
    assert.strictEqual(
      await infer('Destructured', 'entries', 'return Response.json({ imported: entries.length });'),
      IMPORT_BODY
    );
  });

  it('publishes nothing for a member of a body the source states no type for', async () => {
    assert.strictEqual(await infer('UntypedMember', 'raw.items', 'const rows = raw.items;'), undefined);
  });

  // Controls: what is not a member of the parsed body keeps its reading.

  it('keeps reading a locator that names the body binding itself', async () => {
    assert.strictEqual(
      await infer('WholeBinding', 'whole', 'return Response.json({ imported: whole.items.length });'),
      IMPORT_BODY
    );
  });

  it('keeps reading a member of a parameter', async () => {
    assert.strictEqual(
      await infer('ParameterMember', 'context.params', 'const ident = context.params;'),
      '{ id: string; }'
    );
  });

  it('keeps reading a member of an outbound response', async () => {
    assert.strictEqual(await infer('OutboundMember', 'upstream.rate', 'const rate = upstream.rate;'), 'number');
  });
});
