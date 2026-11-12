/**
 * carrick#3897: a consumer call inside a generic wrapper whose result holds
 * the wrapper's own type parameter (`send<T>(...)` returned from
 * `readWidget<T>(): Promise<T>`) does not state the response. The call site
 * cannot bind that parameter: its real reads are at the wrapper's callers.
 * Stating the producer's type there makes the compiler compare a concrete
 * type with the open `T` ("'T' could be instantiated with an arbitrary
 * type"), which is no read of the response at all. The retype abstains.
 *
 * `client.ts`, `send.ts` and `inventory.ts` mirror the web side of the
 * `retype-reach-mono` fixture the issue names, inline, so the case is pinned
 * on its own. The rest are variants of the same shape and controls whose
 * result holds no type parameter of an enclosing declaration.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SEND = `export type RequestOptions = {
  method: string;
};

export async function send<T = any>(url: string, options: RequestOptions): Promise<T> {
  const response = await fetch(url, options);
  return (await response.json()) as T;
}

export async function sendMode<T = any, M extends boolean = false>(
  url: string,
  options: RequestOptions & { strict?: M }
): Promise<T> {
  const response = await fetch(url, options);
  return (await response.json()) as T;
}
`;

const CLIENT = `import { send } from "./send.js";

export class CatalogClient {
  constructor(private readonly baseUrl: string) {}

  readWidget<T = any>(widgetId: string): Promise<T> {
    const encoded = encodeURIComponent(widgetId);
    return send<T>(\`\${this.baseUrl}/api/v1/widgets/\${encoded}\`, {
      method: "GET",
    });
  }
}
`;

const INVENTORY = `import type { CatalogClient } from "./client.js";

export async function countActive(
  widgetId: string,
  client: CatalogClient
): Promise<number> {
  const widget = await client.readWidget(widgetId);
  return widget.activeCount;
}
`;

const VARIANTS = `import { send, sendMode } from "./send.js";

export interface Widget {
  id: string;
  activeCount: number;
}

export interface Envelope<T> {
  data: T;
}

export async function awaited<T>(id: string): Promise<T> {
  return await send<T>('/awaited/' + id, { method: "GET" });
}

export function inferred<T>(id: string): Promise<T> {
  return send('/inferred/' + id, { method: "GET" });
}

export function bounded<T extends Widget>(id: string): Promise<T> {
  return send<T>('/bounded/' + id, { method: "GET" });
}

export function enveloped<T>(id: string): Promise<Envelope<T>> {
  return send<Envelope<T>>('/enveloped/' + id, { method: "GET" });
}

export async function bound<T>(id: string): Promise<T> {
  const value = await send<T>('/bound/' + id, { method: "GET" });
  return value;
}

export class Repo<T> {
  read(id: string): Promise<T> {
    return send<T>('/repo/' + id, { method: "GET" });
  }
}

export function concrete(id: string): Promise<Widget> {
  return send<Widget>('/concrete/' + id, { method: "GET" });
}

export async function insideGeneric<T>(id: string, extra: T): Promise<number> {
  const widget = await send<Widget>('/inside/' + id, { method: "GET" });
  return widget.activeCount;
}

export async function fetched<T>(id: string): Promise<T> {
  const res = await fetch('/fetched/' + id);
  return res.json();
}

export async function fetchedCast<T>(id: string): Promise<number> {
  const res = await fetch('/fetchedCast/' + id);
  const body = (await res.json()) as T;
  return body ? 1 : 0;
}

export async function fetchedWidget<T>(id: string, extra: T): Promise<number> {
  const res = await fetch('/fetchedWidget/' + id);
  const body: Widget = await res.json();
  return body.activeCount;
}

export async function moded<M extends boolean>(id: string, strict: M): Promise<number> {
  const widget = await sendMode<Widget, M>('/moded/' + id, { method: "GET", strict });
  return widget.activeCount;
}

declare function wrap<T>(value: T): T;

export async function outerBinds(id: string): Promise<number> {
  return wrap((await send<Widget>('/outer/' + id, { method: "GET" })).activeCount);
}
`;

interface Outcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'abstain' | 'wider';
  diagnostics: Array<{ line: number; code: number; message: string }>;
  reason?: string;
}

/** The 1-based line of the one `source` line holding `marker`. */
const lineIn = (source: string, marker: string): number => {
  const lines = source.split('\n');
  const at = lines.findIndex((line) => line.includes(marker));
  assert.ok(at >= 0 && lines.findIndex((l, i) => i > at && l.includes(marker)) < 0, marker);
  return at + 1;
};

const sendAt = (route: string) => {
  const marker = `send<T>('/${route}/'`;
  const inferredMarker = `send('/${route}/'`;
  const generic = `send<Envelope<T>>('/${route}/'`;
  const widget = `send<Widget>('/${route}/'`;
  const moded = `sendMode<Widget, M>('/${route}/'`;
  const text = [marker, inferredMarker, generic, widget, moded].find((m) => VARIANTS.includes(m))!;
  const line = lineIn(VARIANTS, text);
  // The call's full text on its one line, up to the closing parenthesis.
  const full = VARIANTS.split('\n')[line - 1];
  const start = full.indexOf(text);
  const end = full.indexOf('})', start) + 2;
  return { line, text: full.slice(start, end) };
};

const WIDGET = '{ id: string; name: string; activeCount: number; }';
const MISSING = '{ id: string; name: string; active: number; }';
const UNBOUND = /holds the type parameter 'T' of the declaration it is in/;

describe('carrick#3897: a call whose result holds an enclosing type parameter', () => {
  let client: SidecarClient;
  let repoDir: string;
  const files: Record<string, string> = {};

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3897-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
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
    for (const [name, text] of Object.entries({
      'send.ts': SEND,
      'client.ts': CLIENT,
      'inventory.ts': INVENTORY,
      'variants.ts': VARIANTS,
    })) {
      files[name] = path.join(repoDir, 'src', name);
      fs.writeFileSync(files[name], text);
    }
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function retype(
    file: string,
    at: { line: number; text: string },
    producerType: string
  ): Promise<Outcome> {
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
      action: 'retype_check',
      request_id: `${file}-${at.line}-${producerType}`,
      items: [
        {
          item_id: `${file}:${at.line}`,
          file_path: files[file],
          line_number: at.line,
          expression_text: at.text,
          expression_line: at.line,
          producer_type: producerType,
          wire: true,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    assert.strictEqual(res.outcomes?.length, 1);
    return res.outcomes![0];
  }

  async function prescreen(file: string, at: { line: number; text: string }): Promise<{ reason?: string }> {
    const res = await client.send<{ status: string; prescreens?: Array<{ reason?: string }> }>({
      action: 'retype_prescreen',
      request_id: `prescreen-${file}-${at.line}`,
      items: [
        {
          item_id: `${file}:${at.line}`,
          file_path: files[file],
          line_number: at.line,
          expression_text: at.text,
          expression_line: at.line,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    return res.prescreens![0];
  }

  // The fixture's wrapper site: `send<T>(...)` on client.ts line 8.
  const wrapperSite = {
    line: lineIn(CLIENT, 'return send<T>('),
    text: 'send<T>(`${this.baseUrl}/api/v1/widgets/${encoded}`, {\n      method: "GET",\n    })',
  };

  it('(fixture client.ts:8) abstains with the reason, whatever the producer returns', async () => {
    assert.strictEqual(wrapperSite.line, 8);
    for (const producer of [WIDGET, MISSING]) {
      const out = await retype('client.ts', wrapperSite, producer);
      assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
      assert.match(out.reason ?? '', UNBOUND);
    }
  });

  it('(fixture client.ts:8) the prescreen gives the same reason before any rewrite', async () => {
    const out = await prescreen('client.ts', wrapperSite);
    assert.match(out.reason ?? '', UNBOUND, JSON.stringify(out));
  });

  for (const route of ['awaited', 'inferred', 'bounded', 'enveloped', 'repo']) {
    it(`(${route}) abstains: the result holds an enclosing type parameter`, async () => {
      const out = await retype('variants.ts', sendAt(route), WIDGET);
      assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
      assert.match(out.reason ?? '', UNBOUND);
    });
  }

  it('(bound) abstains: the value bound from the call is returned as T', async () => {
    const out = await retype('variants.ts', sendAt('bound'), WIDGET);
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', UNBOUND);
  });

  for (const route of ['fetched', 'fetchedCast']) {
    it(`(${route}) abstains: a body read returned as T`, async () => {
      const text = `fetch('/${route}/' + id)`;
      const out = await retype('variants.ts', { line: lineIn(VARIANTS, text), text }, WIDGET);
      assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
      assert.match(out.reason ?? '', UNBOUND);
    });
  }

  it('(fetchedWidget) still judges a body read inside a generic function', async () => {
    const text = "fetch('/fetchedWidget/' + id)";
    const at = { line: lineIn(VARIANTS, text), text };
    const mismatch = await retype('variants.ts', at, MISSING);
    assert.strictEqual(mismatch.outcome, 'mismatch', JSON.stringify(mismatch));
    const agrees = await retype('variants.ts', at, WIDGET);
    assert.strictEqual(agrees.outcome, 'agrees', JSON.stringify(agrees));
  });

  for (const route of ['concrete', 'inside', 'outer', 'moded']) {
    it(`(${route}) still judges a call whose stated type argument holds no enclosing type parameter`, async () => {
      const mismatch = await retype('variants.ts', sendAt(route), MISSING);
      assert.strictEqual(mismatch.outcome, 'mismatch', JSON.stringify(mismatch));
      assert.strictEqual(mismatch.diagnostics.length, 1, JSON.stringify(mismatch));
      const agrees = await retype('variants.ts', sendAt(route), WIDGET);
      assert.strictEqual(agrees.outcome, 'agrees', JSON.stringify(agrees));
    });
  }
});
