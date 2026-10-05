/**
 * `resolve_definitions` reads a capture stub and nothing else (carrick#1927).
 *
 * The resolver builds a project of its own over the stub's `types/` tree. The
 * service's project, the one `init` names and `infer` reads, has no part in
 * the answer, so the request must not build it. It used to: the handler took
 * its resolver from the project's components, and the first request to do so
 * in a process builds the whole program. The scanner asks each capture of a
 * fresh process, so the `resolve_definitions` after it was always that first
 * request, and paid for a program the size of the service to read a tree the
 * size of its surface.
 *
 * Three processes answer the same request over the same stub:
 *  - one that was never `init`'d,
 *  - one that was `init`'d and asked nothing else,
 *  - one that had built the service's project before it was asked.
 * All three give the same definitions, and only the third ever builds.
 *
 * The log is read after the process has closed, so a line it wrote cannot be
 * missed and a line it did not write cannot arrive later.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { spawn } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as readline from 'node:readline';
import { SIDECAR_PATH } from './helpers.js';

interface Answer {
  request_id: string;
  status: string;
  definitions?: Array<{ type_alias: string; definition: string; expanded: string }>;
  inferred_types?: Array<{ alias: string; type_string: string }>;
  errors?: string[];
}

/** What the project loader logs each time it builds a program. */
const PROJECT_BUILT = 'Project built in';

const ORDER = 'Endpoint_order_Response';
const ORDERS = 'Endpoint_orders_Response';

const MODEL_DTS = `export interface Money {
    amountCents: number;
    currency: string;
}
export type OrderStatus = {
    kind: "placed";
    placedAt: string;
} | {
    kind: "refunded";
    refundedAt: string;
    reason?: string;
};
export interface Order {
    id: string;
    total: Money;
    status: OrderStatus;
    note?: string;
}
`;

const ORDER_EXPANDED =
  '{ id: string; total: { amountCents: number; currency: string; }; status: { kind: "placed"; placedAt: string; } | { kind: "refunded"; refundedAt: string; reason?: string; }; note?: string; }';

/** The answer every process must give, whatever it built before. */
const EXPECTED = [
  {
    type_alias: ORDER,
    definition: `export interface Order {
    id: string;
    total: Money;
    status: OrderStatus;
    note?: string;
}`,
    expanded: ORDER_EXPANDED,
  },
  {
    type_alias: ORDERS,
    definition: `export type ${ORDERS} = import('./model').Order[];`,
    expanded: `${ORDER_EXPANDED}[]`,
  },
];

const SERVICE_SOURCE = `export interface Receipt {
  id: string;
  total: number;
}
export function receipt(): Receipt {
  return { id: 'a', total: 1 };
}
`;

/**
 * Run one sidecar process through `requests`, one at a time, then shut it
 * down. Returns each request's terminal answer and everything the process
 * logged in its whole life.
 */
async function session(
  requests: Array<Record<string, unknown>>,
): Promise<{ answers: Answer[]; log: string }> {
  const child = spawn('node', [SIDECAR_PATH], { stdio: ['pipe', 'pipe', 'pipe'] });
  let log = '';
  child.stderr.on('data', (chunk: Buffer) => {
    log += chunk.toString();
  });
  const closed = new Promise<void>((resolve) => child.on('close', () => resolve()));

  const frames: Answer[] = [];
  let arrived: (() => void) | null = null;
  readline.createInterface({ input: child.stdout }).on('line', (line) => {
    if (!line.trim()) return;
    const frame = JSON.parse(line) as Answer;
    // A progress frame is not an answer (carrick#1914).
    if (frame.status === 'progress') return;
    frames.push(frame);
    arrived?.();
  });

  const answers: Answer[] = [];
  try {
    for (const request of [...requests, { action: 'shutdown', request_id: 'shutdown' }]) {
      const expected = frames.length + 1;
      child.stdin.write(JSON.stringify(request) + '\n');
      while (frames.length < expected) {
        await new Promise<void>((resolve, reject) => {
          const timer = setTimeout(
            () => reject(new Error(`no answer to ${String(request.request_id)}`)),
            60_000,
          );
          arrived = () => {
            clearTimeout(timer);
            resolve();
          };
        });
      }
      arrived = null;
      answers.push(frames[expected - 1]);
    }
    await closed;
  } finally {
    child.kill();
  }
  // The shutdown's own answer is not one of the caller's.
  return { answers: answers.slice(0, requests.length), log };
}

describe('resolve_definitions builds no service project (carrick#1927)', () => {
  let serviceRoot: string;
  let stubDir: string;
  let resolve: Record<string, unknown>;
  let init: Record<string, unknown>;

  before(() => {
    // A service with a program to build: a tsconfig and a source file.
    serviceRoot = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-stateless-svc-')));
    fs.mkdirSync(path.join(serviceRoot, 'src'));
    fs.writeFileSync(
      path.join(serviceRoot, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
        include: ['src/**/*.ts'],
      }),
    );
    fs.writeFileSync(path.join(serviceRoot, 'src', 'work.ts'), SERVICE_SOURCE);

    // A stub as a capture writes it: a surface of import-type aliases over a
    // declaration tree, outside the service.
    stubDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-stateless-stub-')));
    const types = path.join(stubDir, 'types');
    fs.mkdirSync(types);
    fs.writeFileSync(path.join(types, 'model.d.ts'), MODEL_DTS);
    fs.writeFileSync(
      path.join(types, 'surface.d.ts'),
      [
        `export type ${ORDER} = import('./model').Order;`,
        `export type ${ORDERS} = import('./model').Order[];`,
        '',
      ].join('\n'),
    );

    init = { action: 'init', request_id: 'init', repo_root: serviceRoot };
    resolve = {
      action: 'resolve_definitions',
      request_id: 'resolve',
      stub_dir: stubDir,
      aliases: [ORDER, ORDERS],
    };
  });

  after(() => {
    fs.rmSync(serviceRoot, { recursive: true, force: true });
    fs.rmSync(stubDir, { recursive: true, force: true });
  });

  it('answers in a process that was never initialised', async () => {
    const { answers, log } = await session([resolve]);
    assert.strictEqual(answers[0].status, 'success', JSON.stringify(answers[0].errors));
    assert.deepStrictEqual(answers[0].definitions, EXPECTED);
    assert.ok(!log.includes(PROJECT_BUILT), `a project was built:\n${log}`);
  });

  it('builds nothing in a process that was only initialised', async () => {
    const { answers, log } = await session([init, resolve]);
    assert.strictEqual(answers[0].status, 'ready', JSON.stringify(answers[0].errors));
    assert.strictEqual(answers[1].status, 'success', JSON.stringify(answers[1].errors));
    assert.deepStrictEqual(answers[1].definitions, EXPECTED);
    assert.ok(!log.includes(PROJECT_BUILT), `the service's project was built:\n${log}`);
  });

  it('answers the same in a process that had built the service project', async () => {
    const { answers, log } = await session([
      init,
      {
        action: 'infer',
        request_id: 'infer',
        requests: [
          {
            file_path: path.join(serviceRoot, 'src', 'work.ts'),
            line_number: SERVICE_SOURCE.split('\n').findIndex((l) => l.includes('function receipt')) + 1,
            infer_kind: 'function_return',
            alias: 'Receipt_return',
          },
        ],
      },
      resolve,
    ]);
    // The line the other two cases look for is the one a build writes, and it
    // is written once: by the inference, not again by the resolve.
    assert.strictEqual(answers[1].status, 'success', JSON.stringify(answers[1].errors));
    assert.strictEqual(log.split(PROJECT_BUILT).length - 1, 1, `expected one build:\n${log}`);
    assert.strictEqual(answers[2].status, 'success', JSON.stringify(answers[2].errors));
    assert.deepStrictEqual(answers[2].definitions, EXPECTED);
  });
});
