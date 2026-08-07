/**
 * carrick#498: a subscriber anchor must capture the payload its handler
 * RECEIVES, not the type of the registration call it sits on.
 *
 * Pre-fix, the subscriber side of a pub/sub pair reached capture as a
 * LINE-ONLY infer anchor (its collector deliberately sends no expression
 * text). `locateNode`'s line fallback then resolved the first expression on
 * that line, which is the enclosing `client.subscribe(...)` CALL — so the
 * alias captured that call's return type. With a `void`-returning subscribe
 * the surface line read `void`; with one returning a subscription handle it
 * read that handle. Both self-check clean, so nothing downstream caught it and
 * every correctly-typed publisher/subscriber pair read incompatible.
 *
 * The fix carries the `function_param` locator through to capture as
 * `param_name` and resolves the handler's parameter structurally: the last
 * function-typed argument of the call on the anchor's line (or a function
 * declared within two lines of it), then the parameter by name, by whole
 * destructured binding pattern, or by binding element. No method name,
 * library, or topic string is consulted anywhere.
 *
 * Fixtures are synthetic and generically named.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

let repoDir: string;
let outRoot: string;

const SUBSCRIBER_TS = `export interface OrderEvent {
  orderId: string;
  total: number;
}

export interface Envelope {
  receivedAt: string;
  body: OrderEvent;
}

export interface Subscription {
  readonly id: string;
  close(): void;
}

interface VoidBroker {
  subscribe(topic: string, handler: (msg: unknown) => void | Promise<void>): void;
}

interface HandleBroker {
  subscribe(topic: string, handler: (msg: unknown) => void | Promise<void>): Subscription;
}

declare const voidClient: VoidBroker;
declare const handleClient: HandleBroker;

// 1. Plain named parameter on an inline handler.
voidClient.subscribe('orders.placed', async (msg: OrderEvent) => {
  console.log(msg.orderId);
});

// 2. Whole destructured binding pattern: the pattern's own type IS the payload.
voidClient.subscribe('orders.shipped', async ({ orderId, total }: OrderEvent) => {
  console.log(orderId, total);
});

// 3. One binding element inside a destructured envelope parameter.
voidClient.subscribe('orders.settled', async ({ body }: Envelope) => {
  console.log(body.orderId);
});

// 4. The registration returns a handle rather than void: pre-fix this captured
//    the handle, which is the same bug wearing a non-void type.
handleClient.subscribe('orders.cancelled', async (msg: OrderEvent) => {
  console.log(msg.orderId);
});

// 5. Handler signature starts a line below the registration call.
voidClient.subscribe(
  'orders.refunded',
  async (msg: OrderEvent) => {
    console.log(msg.total);
  }
);

// 6. Two registrations nested on one line, each with a \`msg\` parameter. The
//    anchor's line denotes the INNER one; the outer handler takes a different
//    payload, so picking it would be a confident wrong answer.
declare function wrap(value: void, after: (msg: Envelope) => void): void;
wrap(voidClient.subscribe('orders.nested', async (msg: OrderEvent) => { console.log(msg.total); }), (msg: Envelope) => { console.log(msg.receivedAt); });

// 7. No such parameter anywhere: must abstain, never fall back to the call.
voidClient.subscribe('orders.archived', async (msg: OrderEvent) => {
  console.log(msg.orderId);
});
`;

function writeRepo(): void {
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
        skipLibCheck: true,
      },
      include: ['src'],
    })
  );
  fs.writeFileSync(path.join(repoDir, 'src', 'subscriber.ts'), SUBSCRIBER_TS);
}

/** 1-based line of the first source line containing `needle`. */
function lineOf(needle: string): number {
  const lines = SUBSCRIBER_TS.split('\n');
  const index = lines.findIndex((l) => l.includes(needle));
  assert.ok(index >= 0, `fixture line not found: ${needle}`);
  return index + 1;
}

describe('capture v2: subscriber anchors capture the handler payload parameter (carrick#498)', () => {
  let result: CaptureStubResult;
  let surface: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-498-repo-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-498-stub-'));
    writeRepo();
    const anchor = (alias: string, line: number, param: string) => ({
      kind: 'infer' as const,
      alias,
      source_file: 'src/subscriber.ts',
      anchor_origin: 'deterministic-infer' as const,
      line_number: line,
      param_name: param,
    });
    result = captureStub({
      repoRoot: repoDir,
      serviceName: 'subscriber-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: [
        anchor('Plain_Producer_Response', lineOf("'orders.placed'"), 'msg'),
        anchor(
          'Pattern_Producer_Response',
          lineOf("'orders.shipped'"),
          '{ orderId, total }'
        ),
        anchor('Element_Producer_Response', lineOf("'orders.settled'"), 'body'),
        anchor('Handle_Producer_Response', lineOf("'orders.cancelled'"), 'msg'),
        // Anchored on the topic argument line; the handler starts one line below,
        // inside the tolerance the fix mirrors from the v1 inferrer.
        anchor('NextLine_Producer_Response', lineOf("'orders.refunded'"), 'msg'),
        anchor('Nested_Producer_Response', lineOf("'orders.nested'"), 'msg'),
        anchor('Missing_Producer_Response', lineOf("'orders.archived'"), 'nosuchparam'),
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(
      path.join(result.stub_dir, 'types', 'surface.d.ts'),
      'utf8'
    );
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  const aliasLine = (alias: string): string => {
    const match = surface.match(new RegExp(`export type ${alias} = ([^;]+);`));
    assert.ok(match, `no surface line for ${alias} in:\n${surface}`);
    return match[1].trim();
  };

  it('a plain named handler parameter captures the payload, not the call', () => {
    assert.match(aliasLine('Plain_Producer_Response'), /OrderEvent/);
  });

  it('a whole destructured binding pattern captures the payload', () => {
    const text = aliasLine('Pattern_Producer_Response');
    assert.match(text, /OrderEvent|orderId/);
    assert.doesNotMatch(text, /^void$/);
  });

  it('a binding element inside an envelope parameter captures that property', () => {
    assert.match(aliasLine('Element_Producer_Response'), /OrderEvent/);
  });

  it('a registration returning a handle still captures the payload', () => {
    const text = aliasLine('Handle_Producer_Response');
    assert.match(text, /OrderEvent/);
    assert.doesNotMatch(text, /Subscription/);
  });

  it('a handler whose signature starts below the registration line resolves', () => {
    assert.match(aliasLine('NextLine_Producer_Response'), /OrderEvent/);
  });

  it('same-line nested registrations resolve the innermost handler', () => {
    const text = aliasLine('Nested_Producer_Response');
    assert.match(text, /OrderEvent/);
    assert.doesNotMatch(text, /Envelope|receivedAt/);
  });

  it('no surface line captures the registration call type', () => {
    for (const alias of [
      'Plain_Producer_Response',
      'Pattern_Producer_Response',
      'Element_Producer_Response',
      'Handle_Producer_Response',
      'NextLine_Producer_Response',
    ]) {
      assert.notStrictEqual(
        aliasLine(alias),
        'void',
        `${alias} captured the registration call's return type`
      );
    }
  });

  it('an unresolvable parameter abstains rather than capturing the call', () => {
    assert.strictEqual(aliasLine('Missing_Producer_Response'), 'unknown');
    const record = result.aliases.find(
      (a) => a.alias === 'Missing_Producer_Response'
    );
    assert.ok(record?.capture_failure_reason?.includes('nosuchparam'));
  });
});
