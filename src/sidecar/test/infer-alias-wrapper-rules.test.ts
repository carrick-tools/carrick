/**
 * carrick#1843: a wrapper rule that names a type alias unwraps it.
 *
 * A type goes by up to two names: the symbol of the declaration its structure
 * comes from, and the alias it was written through. The compiler states each
 * with its own argument list. The matcher read the own symbol only, and the
 * payload read the own arguments only, so a rule naming an alias never
 * unwrapped one:
 *
 *  - an alias of a class has the class's symbol, so the rule's name missed;
 *  - an alias of an object literal has the anonymous `__type`, so the name
 *    missed, and the literal has no arguments of its own;
 *  - an alias of a union matched by name, having no own symbol to hide it,
 *    and then read no payload, a union having no arguments of its own.
 *
 * A rule now matches either name, and the payload is the argument stated for
 * the name that matched: `payloadGenericIndex` counts the parameters of the
 * declaration the rule names, which an alias is free to order differently
 * from the type it stands for.
 *
 * What the rules do not decide is left undecided. A named argument that is a
 * top type is no payload, and the next argument along is not read in its
 * place: on a result carrier that is the error side. An alias the rule does
 * not name and its modules do not declare lends no arguments to it.
 *
 * On a result carrier (`Task<Outcome<Reply<T>, E>>`) the rules now reach what
 * the carrier holds before the carrier read does (carrick#1376,
 * carrick#1841). The site answers as that read answered: a payload by its
 * members, anchored on it, and verified transport with no payload as the
 * decided abstain, whether the service has a rule for every layer, for some,
 * or for none of the outer ones.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import { SidecarClient } from './helpers.js';
import {
  WIRE_ORIGIN,
  collapse,
  writeWireRepo,
  type WireInferred,
} from './wire-package.js';

const SERVICE_TS = `import { Wire, type Reply } from "tiny-wire";

export interface Invoice {
  id: string;
  total: number;
}

export function ping() {
  return Wire.make({ url: "/v1/ping", method: "POST", type: "text" });
}

export function sendInvite(id: string) {
  return Wire.make({ url: \`/v1/invitations/\${id}/send\`, method: "POST", type: "json" });
}

export function loadInvoice(id: string) {
  return Wire.typed<Invoice>(\`/v1/invoices/\${id}\`);
}

export function loadUntyped(id: string) {
  return Wire.untyped(\`/v1/untyped/\${id}\`);
}

export function loadTask(id: string) {
  return Wire.task<Invoice>(\`/v1/task/\${id}\`);
}

export function loadOutcome(id: string) {
  return Wire.outcome<Invoice>(\`/v1/outcome/\${id}\`);
}

export function loadReply(id: string) {
  return Wire.reply<Invoice>(\`/v1/reply/\${id}\`);
}

export function loadScoped(id: string) {
  return Wire.scoped<Invoice>(\`/v1/scoped/\${id}\`);
}

type Stamped<Stamp extends string, T> = Reply<T>;
declare function stamped(url: string): Stamped<"v2", Invoice>;

export function loadStamped(id: string) {
  return stamped(\`/v1/stamped/\${id}\`);
}

declare function trimmed(url: string): Omit<Reply<Invoice>, "url">;

export function loadTrimmed(id: string) {
  return trimmed(\`/v1/trimmed/\${id}\`);
}
`;

/** A service's own alias that only shares a library wrapper's name. */
const LOCAL_TS = `export interface Receipt {
  id: string;
  settled: boolean;
}

type Reply<T> = { status: number; ok: boolean; body: T };
declare function localReply(url: string): Reply<Receipt>;

export function loadLocal(id: string) {
  return localReply(\`/v1/local/\${id}\`);
}
`;

/** A service's own alias, named like a rule's wrapper, around a library interface. */
const SHADOW_TS = `import type { Envelope } from "tiny-wire";

export interface Receipt {
  id: string;
  settled: boolean;
}

type Reply<T> = Envelope<T>;
declare function shadowed(url: string): Reply<Receipt>;

export function loadShadowed(id: string) {
  return shadowed(\`/v1/shadowed/\${id}\`);
}
`;

interface Rule {
  wrapperSymbols?: string[];
  machineryIndicators?: string[];
  originModuleGlobs?: string[];
  payloadGenericIndex?: number;
  unwrapRecursively?: boolean;
  maxDepth?: number;
}

const rule = (name: string, extra: Partial<Rule> = {}): Rule => ({
  wrapperSymbols: [name],
  originModuleGlobs: WIRE_ORIGIN,
  payloadGenericIndex: 0,
  unwrapRecursively: true,
  maxDepth: 4,
  ...extra,
});

/** The library response object's rule, shaped like the one a scan receives. */
const REPLY_RULE = rule('Reply', {
  machineryIndicators: ['headers', 'status', 'statusText', 'ok', 'body'],
});

/** A rule for each of the three aliases a request's result is written with. */
const ALL_THREE = [REPLY_RULE, rule('Task'), rule('Outcome')];

interface InferShape {
  inferred_types?: WireInferred[];
}

describe('carrick#1843: a wrapper rule that names a type alias unwraps it', () => {
  let client: SidecarClient;
  let repoDir: string;
  let pathOf: (name: string) => string;

  before(async () => {
    ({ repoDir, pathOf } = writeWireRepo('carrick-1843-', {
      'service.ts': SERVICE_TS,
      'local.ts': LOCAL_TS,
      'shadow.ts': SHADOW_TS,
    }));
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /**
   * Infer the call whose text starts with `callee` in `file`, with the
   * locator a scan sends (the call's text and line) and the given rules.
   */
  async function infer(
    alias: string,
    callee: string,
    rules: Rule[],
    file: { name: string; text: string } = { name: 'service.ts', text: SERVICE_TS }
  ): Promise<WireInferred> {
    const lines = file.text.split('\n');
    const index = lines.findIndex((line) => line.includes(callee));
    assert.ok(index >= 0, `the fixture must hold a call to ${callee}`);
    const expression = lines[index].trim().replace(/^return /, '').replace(/;$/, '');
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: pathOf(file.name),
          line_number: index + 1,
          infer_kind: 'call_result',
          alias,
          expression_text: expression,
          expression_line: index + 1,
        },
      ],
      extraction_config: { rules },
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, 'the row must be answered');
    return inferred;
  }

  /** `unknown`, said as a decision: transport the rules verify and read nothing out of. */
  function assertDecidedTransport(inferred: WireInferred): void {
    assert.strictEqual(
      collapse(inferred.type_string),
      'unknown',
      `verified transport with no payload states no contract, got: ${inferred.type_string}`
    );
    const root = (inferred.any_provenance ?? []).filter((p) => p.path === '');
    assert.deepStrictEqual(
      root.map((p) => [p.kind, p.reason]),
      [['unknown', 'machinery_envelope']],
      'the abstain must say it was decided, or the capture re-reads the raw call'
    );
    assert.strictEqual(inferred.primary_type_symbol, undefined, 'transport must not anchor');
  }

  describe('a request read through all three aliases', () => {
    it("publishes a typed json request's payload", async () => {
      // `Task<Outcome<Reply<Invoice>, WireError>>`: an alias of a class, of a
      // union and of an object literal, each named by one rule. The payload
      // is what a result carrier holds, so it is printed with its own
      // members and anchors the row, as the carrier read publishes one.
      const inferred = await infer('Typed_Response', 'Wire.typed<Invoice>', ALL_THREE);
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; total: number; }');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
      assert.strictEqual(inferred.raw_text_read, undefined);
    });

    it('publishes a text request as the string it reads, marked as raw text', async () => {
      // `Reply<string>`: the payload is the text of the body, which states no
      // structural contract (carrick#1842). The mark is what keeps `string`
      // from being judged against the other side's JSON body.
      const inferred = await infer('Text_Response', 'type: "text"', ALL_THREE);
      assert.strictEqual(collapse(inferred.type_string), 'string');
      assert.strictEqual(inferred.raw_text_read, true);
      assert.strictEqual(inferred.primary_type_symbol, undefined);
    });

    it('abstains, decided, on a json request whose payload the library leaves open', async () => {
      // `Reply<unknown>`: the rules verify every layer and the last one holds
      // no payload. That is the answer carrick#1841 gave through the carrier
      // read, and it has to stay decided now the rules reach it first: an
      // undecided `unknown` is re-read by the capture, which publishes the
      // library's objects.
      const inferred = await infer('Json_Response', 'type: "json"', ALL_THREE);
      assertDecidedTransport(inferred);
    });

    it('never reads the error side where the payload argument is open', async () => {
      // `Task<Outcome<unknown, WireError>>`: argument 0 is the payload the
      // rule names, and it is `unknown`. Argument 1 is the next one along and
      // the failure type; taking it would publish an error as the body.
      const inferred = await infer('Untyped_Response', 'Wire.untyped', ALL_THREE);
      assertDecidedTransport(inferred);
      assert.doesNotMatch(inferred.type_string, /WireError|url/);
    });
  });

  describe('each form of alias, with its own rule only', () => {
    it('an alias of a class: the rule names the alias, the type has the class symbol', async () => {
      const inferred = await infer('Task_Response', 'Wire.task<Invoice>', [rule('Task')]);
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it('an alias of a union: the arguments are the alias arguments', async () => {
      const inferred = await infer('Outcome_Response', 'Wire.outcome<Invoice>', [rule('Outcome')]);
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; total: number; }');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it('an alias of an object literal: the own symbol is anonymous and has no arguments', async () => {
      const inferred = await infer('Reply_Response', 'Wire.reply<Invoice>', [rule('Reply')]);
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });
  });

  describe('the payload index counts the parameters of the name the rule gives', () => {
    // `Scoped<"billing", Invoice>` is `Envelope<Invoice>`: one own argument,
    // two alias arguments.
    it('a rule naming the alias reads the alias arguments', async () => {
      const inferred = await infer('Scoped_Response', 'Wire.scoped<Invoice>', [
        rule('Scoped', { payloadGenericIndex: 1 }),
      ]);
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it("a rule naming the alias does not read the interface's argument at its index", async () => {
      // Index 0 of `Scoped` is the scope. Reading the own list there would
      // answer `Invoice`, the argument of a declaration the rule did not name.
      const inferred = await infer('ScopedFirst_Response', 'Wire.scoped<Invoice>', [
        rule('Scoped', { payloadGenericIndex: 0 }),
      ]);
      assert.strictEqual(collapse(inferred.type_string), '"billing"');
    });

    it('a rule naming the interface reads its own arguments, as it always did', async () => {
      const inferred = await infer('ScopedOwn_Response', 'Wire.scoped<Invoice>', [
        rule('Envelope'),
      ]);
      assert.strictEqual(collapse(inferred.type_string), 'Invoice');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });
  });

  describe('a service with a rule for the thenable and none for the outcome', () => {
    // The rule for `Task` now matches, and what it leaves is
    // `Outcome<Reply<T>, WireError>`: the carrier the carrier read (#1376)
    // found by shape before the rule could match. It still has its turn, and
    // what it holds still passes the rules (#1841).
    const TASK_AND_REPLY = [REPLY_RULE, rule('Task')];

    it('publishes the payload behind the carrier the rule left, as all three rules do', async () => {
      const inferred = await infer('PartTyped_Response', 'Wire.typed<Invoice>', TASK_AND_REPLY);
      assert.strictEqual(collapse(inferred.type_string), '{ id: string; total: number; }');
      assert.strictEqual(inferred.primary_type_symbol, 'Invoice');
    });

    it('publishes a text request as a marked string', async () => {
      const inferred = await infer('PartText_Response', 'type: "text"', TASK_AND_REPLY);
      assert.strictEqual(collapse(inferred.type_string), 'string');
      assert.strictEqual(inferred.raw_text_read, true);
    });

    it('abstains, decided, where the carrier holds verified transport', async () => {
      const inferred = await infer('PartJson_Response', 'type: "json"', TASK_AND_REPLY);
      assertDecidedTransport(inferred);
    });

    it('never anchors on the carrier the rule left', async () => {
      // `Outcome<unknown, WireError>`: the carrier holds no payload to print,
      // so the row keeps the carrier as written (#1376). Its alias is not
      // the operation's type and must not be the row's anchor.
      const inferred = await infer('PartUntyped_Response', 'Wire.untyped', [rule('Task')]);
      assert.match(inferred.type_string, /<unknown, /);
      assert.strictEqual(inferred.primary_type_symbol, undefined);
    });
  });

  describe('what a rule does not name or its modules do not declare', () => {
    it("lends no arguments from a service's alias around the library's object", async () => {
      // `Stamped<"v2", Invoice>` is the library's response object by its
      // members and origin, written through an alias the service declares.
      // The rule's index says nothing about that alias's parameters, and its
      // first one is not the payload. The rule verifies the object and reads
      // nothing out of it, as it did before a rule read alias arguments.
      const inferred = await infer('Stamped_Response', 'stamped(`', [REPLY_RULE]);
      assert.strictEqual(collapse(inferred.type_string), 'unknown');
      assert.strictEqual(inferred.primary_type_symbol, undefined);
      // The result is no carrier, so the answer is the one it had: undecided,
      // which leaves the capture's own read of the call its turn.
      assert.deepStrictEqual(
        (inferred.any_provenance ?? []).filter((p) => p.path === ''),
        [],
        'only a carrier of verified transport is a decided abstain'
      );
    });

    it('lends no arguments from a utility alias around the transport', async () => {
      // `Omit<Reply<Invoice>, "url">` has the transport's members, and is
      // written through an alias whose first argument is the transport
      // itself. A rule whose modules include the utility's home (as a rule
      // for a platform type does) named neither name, so it reads no alias
      // argument: argument 0 here is the response object, not a body.
      const inferred = await infer('Trimmed_Response', 'trimmed(`', [
        {
          wrapperSymbols: ['Reply'],
          machineryIndicators: ['headers', 'status', 'statusText', 'ok', 'body'],
          originModuleGlobs: [...WIRE_ORIGIN, 'typescript/lib/*'],
          unwrapRecursively: false,
        },
      ]);
      assert.strictEqual(collapse(inferred.type_string), 'unknown');
      assert.strictEqual(inferred.primary_type_symbol, undefined);
    });

    it("leaves a service's own alias alone when the rule's modules do not declare it", async () => {
      const inferred = await infer('LocalGated_Response', 'localReply(`', [rule('Reply')], {
        name: 'local.ts',
        text: LOCAL_TS,
      });
      assert.match(inferred.type_string, /status: number/);
      assert.match(inferred.type_string, /body:/);
    });

    it("still matches by members and origin under a service's alias that shares the rule's name", async () => {
      // `Reply<Receipt>` here is the library's `Envelope<Receipt>` written
      // through the service's own `Reply`. The rule's name matches that
      // alias, which its modules do not declare, so the name is no match.
      // The rule's members-and-origin test still has its turn, as it did
      // before a rule could match an alias: the interface is the library's,
      // and its own argument is the payload.
      const inferred = await infer(
        'Shadowed_Response',
        'shadowed(`',
        [rule('Reply', { machineryIndicators: ['requestId'] })],
        { name: 'shadow.ts', text: SHADOW_TS }
      );
      assert.strictEqual(collapse(inferred.type_string), 'Receipt');
      assert.strictEqual(inferred.primary_type_symbol, 'Receipt');
    });

    it("unwraps a service's own alias for a rule that names it with no origin", async () => {
      const inferred = await infer(
        'LocalNamed_Response',
        'localReply(`',
        [{ wrapperSymbols: ['Reply'], payloadGenericIndex: 0, unwrapRecursively: true }],
        { name: 'local.ts', text: LOCAL_TS }
      );
      assert.strictEqual(collapse(inferred.type_string), 'Receipt');
      assert.strictEqual(inferred.primary_type_symbol, 'Receipt');
    });
  });
});
