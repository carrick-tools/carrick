/**
 * carrick#1516: a response inference carries, beside the type the compiler
 * infers, the type its handler actually returns with no literal widened.
 *
 * One fixture, several shapes that differ in what matters:
 *
 * - a decorated controller method returning a service call, whose service
 *   maps rows to objects with a conditional string and a negative numeric
 *   choice (the literal sits two calls away from the route). The same service
 *   method carries every literal the reading must leave alone: a quoted
 *   property name, a literal type in an annotation, a tagged template, and a
 *   parameter default the controller overrides. Marking any of them adds a
 *   diagnostic and costs the whole reading;
 * - a boolean discriminant across two returns;
 * - an object built with a literal and then assigned another value, which the
 *   reading must NOT narrow (the compiler reports the assignment once the
 *   literal is kept, and the reading is dropped);
 * - a handler whose `let` bindings take literals directly, through a
 *   conditional, and through `||` inside parentheses, and are then reassigned:
 *   none of them is marked (marking one adds a diagnostic and costs the
 *   reading), while the `const` beside them is;
 * - a service method with a DECLARED return type, which is the contract and is
 *   not followed: its body would cost the reading a diagnostic if it were;
 * - a callback route whose payload is located by span and by expression text,
 *   after a marked literal, so the re-read has to find the payload again in
 *   the rewritten file.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { SidecarClient } from './helpers.js';
import { TypeInferrer } from '../src/type-inferrer.js';

const DECORATORS = `export function Get(path: string): MethodDecorator {
  return () => {
    void path;
  };
}
`;

const SERVICE = `export interface HolidayRow {
  id: string;
  parentId: string | null;
  name: string;
  weight: number;
}

declare function sql(parts: TemplateStringsArray): string;

export class HolidaysService {
  private rows: HolidayRow[] = [];

  async listHolidays(order = 'asc') {
    const direction: 'asc' | 'desc' = order === 'desc' ? 'desc' : 'asc';
    const query = sql\`select * from holidays\`;
    const rows = query && direction ? this.rows : [];
    return rows.map((row) => ({
      id: row.id,
      'display-name': row.name,
      scope: row.parentId ? 'specific' : 'all',
      rank: row.weight > 1 ? 2 : -1,
      order,
    }));
  }

  async session(open: boolean) {
    if (!open) return { active: false, session: null };
    return { active: true, session: { id: this.rows.length } };
  }

  relabelled() {
    return this.rows.map((row) => {
      const label = { text: 'draft' };
      if (row.weight > 0) label.text = row.name;
      return label;
    });
  }

  declared(): Promise<Array<{ scope: string }>> {
    const row = { scope: 'all' };
    row.scope = this.rows[0]?.name ?? 'none';
    return Promise.resolve([row]);
  }
}
`;

// Line numbers are read off this text by `lineOf`.
const CONTROLLER = `import { Get } from './decorators';
import { HolidaysService } from './holidays.service';

export class HolidaysController {
  private live = true;

  constructor(private readonly service: HolidaysService) {}

  @Get('holidays')
  async listHolidays() {
    return this.service.listHolidays('desc');
  }

  @Get('session')
  async session() {
    return this.service.session(true);
  }

  @Get('labels')
  relabelled() {
    return this.service.relabelled();
  }

  @Get('totals')
  totals() {
    let total = 0;
    for (const n of [1, 2, 3]) total += n;
    let label = total > 10 ? 'big' : 'small';
    const preset = '' as 'fast' | '';
    let mode = (preset || 'slow');
    if (total === 0) {
      label = 'empty';
      mode = 'idle';
    }
    const kind = total > 3 ? 'many' : 'few';
    return { total, kind, label, mode };
  }

  @Get('declared')
  async declared() {
    return { items: await this.service.declared(), source: this.live ? 'live' : 'cache' };
  }
}
`;

const ROUTES = `interface Res {
  json(body: unknown): void;
}
declare const app: { get(path: string, handler: (req: unknown, res: Res) => void): void };
declare function healthy(): boolean;

app.get('/health', (req, res) => {
  const probe = 'db';
  res.json({ status: healthy() ? 'ok' : 'degraded', checks: 3, probe });
});
`;

interface Inferred {
  alias: string;
  type_string: string;
  unwidened_type_string?: string;
}

function lineOf(text: string, needle: string): number {
  const at = text.indexOf(needle);
  assert.ok(at >= 0, `fixture must contain: ${needle}`);
  return text.slice(0, at).split('\n').length;
}

const collapse = (text: string | undefined): string | undefined => text?.replace(/\s+/g, ' ').trim();

describe('carrick#1516: the unwidened reading of a response inference', () => {
  let client: SidecarClient;
  let repoDir: string;
  let controllerPath: string;
  let routesPath: string;
  let inferred: Map<string, Inferred>;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1516-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          experimentalDecorators: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'decorators.ts'), DECORATORS);
    fs.writeFileSync(path.join(repoDir, 'src', 'holidays.service.ts'), SERVICE);
    controllerPath = path.join(repoDir, 'src', 'holidays.controller.ts');
    fs.writeFileSync(controllerPath, CONTROLLER);
    routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, ROUTES);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });

    const payload = "{ status: healthy() ? 'ok' : 'degraded', checks: 3, probe }";
    const spanStart = ROUTES.indexOf(payload);
    const route = (name: string) => ({
      file_path: controllerPath,
      line_number: lineOf(CONTROLLER, `@Get('${name}')`),
      infer_kind: 'response_body',
      alias: name,
    });
    // One batch, as a scan sends it: the reading rewrites every file once.
    const res = await client.send<{ status: string; inferred_types?: Inferred[]; errors?: string[] }>({
      action: 'infer',
      request_id: 'batch',
      requests: [
        route('holidays'),
        route('session'),
        route('labels'),
        route('totals'),
        route('declared'),
        {
          file_path: routesPath,
          line_number: lineOf(ROUTES, payload),
          infer_kind: 'response_body',
          alias: 'health-span',
          span_start: spanStart,
          span_end: spanStart + payload.length,
        },
        {
          file_path: routesPath,
          line_number: lineOf(ROUTES, payload),
          infer_kind: 'response_body',
          alias: 'health-text',
          expression_text: payload,
          expression_line: lineOf(ROUTES, payload),
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    inferred = new Map((res.inferred_types ?? []).map((t) => [t.alias, t]));
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  it('keeps the published type widened: the compiler inference is the contract', () => {
    const t = inferred.get('holidays');
    assert.ok(t, 'the route must infer');
    assert.match(collapse(t.type_string)!, /scope: string/);
    assert.match(collapse(t.type_string)!, /rank: number/);
  });

  it('follows the controller into the service and reads the literals it returns', () => {
    const t = inferred.get('holidays')!;
    const unwidened = collapse(t.unwidened_type_string);
    assert.ok(unwidened, `a reading is expected, got: ${JSON.stringify(t)}`);
    assert.match(unwidened, /scope: "all" \| "specific"|scope: "specific" \| "all"/);
    assert.match(unwidened, /rank: -1 \| 2|rank: 2 \| -1/);
    assert.match(unwidened, /id: string/, 'a member with no literal keeps its type');
    assert.match(unwidened, /['"]display-name['"]: string/);
    assert.match(unwidened, /order: string/, 'a parameter default is what callers may override');
  });

  it('keeps a boolean discriminant on each return', () => {
    const unwidened = collapse(inferred.get('session')!.unwidened_type_string);
    assert.ok(unwidened, 'a reading is expected');
    assert.match(unwidened, /active: false; session: null/);
    assert.match(unwidened, /active: true; session: \{ id: number; \}/);
  });

  it('drops the reading when a kept literal is later assigned another value', () => {
    const t = inferred.get('labels')!;
    assert.match(collapse(t.type_string)!, /text: string/);
    assert.strictEqual(t.unwidened_type_string, undefined, JSON.stringify(t));
  });

  it('never marks a literal that initialises a mutable binding', () => {
    const t = inferred.get('totals')!;
    const unwidened = collapse(t.unwidened_type_string);
    assert.ok(unwidened, `the let bindings must not cost the reading: ${JSON.stringify(t)}`);
    assert.match(unwidened, /total: number/);
    assert.match(unwidened, /label: string/);
    assert.match(unwidened, /mode: string/);
    assert.match(unwidened, /kind: "many" \| "few"|kind: "few" \| "many"/, 'a const is marked');
  });

  it('does not follow a callee whose return type is declared', () => {
    const t = inferred.get('declared')!;
    assert.match(collapse(t.type_string)!, /scope: string/);
    const unwidened = collapse(t.unwidened_type_string);
    assert.ok(unwidened, `the declared callee must not cost the reading: ${JSON.stringify(t)}`);
    assert.match(unwidened, /scope: string/, 'the declared type is the contract');
    assert.match(unwidened, /source: "live" \| "cache"|source: "cache" \| "live"/);
  });

  for (const alias of ['health-span', 'health-text']) {
    it(`finds a payload located by ${alias.slice('health-'.length)} again in the rewritten file`, () => {
      const t = inferred.get(alias);
      assert.ok(t, 'the payload must infer');
      assert.match(collapse(t.type_string)!, /status: string/);
      const unwidened = collapse(t.unwidened_type_string);
      assert.ok(unwidened, `a reading is expected, got: ${JSON.stringify(t)}`);
      assert.match(unwidened, /status: "ok" \| "degraded"|status: "degraded" \| "ok"/);
      assert.match(unwidened, /checks: 3/);
      // Marked before the payload, so its span has to move to be found again.
      assert.match(unwidened, /probe: "db"/);
    });
  }

  it('reads nothing once its budget is spent, and still answers', () => {
    const request = {
      file_path: controllerPath,
      line_number: lineOf(CONTROLLER, "@Get('holidays')"),
      infer_kind: 'response_body' as const,
      alias: 'holidays',
    };
    const read = (unwidenedBudgetMs?: number) => {
      const project = new Project({ tsConfigFilePath: path.join(repoDir, 'tsconfig.json') });
      const inferrer = new TypeInferrer({ project, repoRoot: repoDir, unwidenedBudgetMs });
      return inferrer.infer([request]).inferred_types?.[0];
    };
    const spent = read(0);
    assert.ok(spent, 'the inference itself is not the reading and must answer');
    assert.match(collapse(spent.type_string)!, /scope: string/);
    assert.strictEqual(spent.unwidened_type_string, undefined);
    // Control: the same inference with the default budget reads.
    assert.ok(read()?.unwidened_type_string);
  });

  it('restores every file it rewrote', async () => {
    // A second batch over the same project reads the original text: the
    // published types are unchanged and the payload span still locates.
    const payload = "{ status: healthy() ? 'ok' : 'degraded', checks: 3, probe }";
    const spanStart = ROUTES.indexOf(payload);
    const res = await client.send<{ status: string; inferred_types?: Inferred[] }>({
      action: 'infer',
      request_id: 'again',
      requests: [
        {
          file_path: routesPath,
          line_number: lineOf(ROUTES, payload),
          infer_kind: 'response_body',
          alias: 'again',
          span_start: spanStart,
          span_end: spanStart + payload.length,
        },
      ],
    });
    const t = res.inferred_types?.[0];
    assert.ok(t, JSON.stringify(res));
    assert.strictEqual(collapse(t.type_string), collapse(inferred.get('health-span')!.type_string));
    assert.strictEqual(
      collapse(t.unwidened_type_string),
      collapse(inferred.get('health-span')!.unwidened_type_string)
    );
  });
});
