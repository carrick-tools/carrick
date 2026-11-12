/**
 * Regression for carrick#3741: a client method answers a one-parameter result
 * envelope, `{ success: true; data: T } | { success: false; error: string }`,
 * and the call site reads only the envelope's own status and error members.
 * The consumer row published the envelope as the response type, and judged
 * against a producer that answers `T` it read as a break nobody can prove.
 *
 * The envelope is the client's bookkeeping, not the wire body, and a site
 * that never reads the payload member states nothing about the payload. It
 * abstains with the decision carrick#1375 records for a site that reads only
 * parts of a value (`projected_value_only`), so a sibling site for the same
 * operation can answer.
 *
 * Shape-only: a union whose branches hold the envelope's single type argument
 * as a member. No name, library or package is matched.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
const SERVICE_TS = `type ClientResult<T> =
  | { success: true; data: T }
  | { success: false; error: string; statusCode?: number };

declare function closeTicket(id: string): Promise<ClientResult<{ ticketId: string }>>;
declare function publishBuild(
  id: string,
  options: { tag: string },
  onLog: (line: string) => void
): Promise<ClientResult<{ buildId: string }>>;

export async function close(id: string): Promise<void> {
  const res = await closeTicket(id);
  if (!res.success) throw new Error(res.error);
}

export async function closeAndRead(id: string): Promise<string> {
  const res = await closeTicket(id);
  if (!res.success) throw new Error(res.error);
  return res.data.ticketId;
}

export async function publish(id: string, tag: string): Promise<void> {
  const res = await publishBuild(
    id,
    { tag },
    (line) => {
      console.log(line);
    },
  );
  if (!res.success) throw new Error(res.error);
}

type SendOutcome = { ok: true } | { ok: false; reason: string };
declare function sendConfirmation(address: string): Promise<SendOutcome>;

export async function confirm(address: string): Promise<boolean> {
  const sent = await sendConfirmation(address);
  if (!sent.ok) return false;
  return true;
}
`;
/** 1-based lines in SERVICE_TS, read off the source above. */
const STATUS_ONLY_LINE = 14;
const PAYLOAD_READ_LINE = 19;
const MULTI_LINE_CALL_LINE = 25;
const NON_GENERIC_LINE = 39;
const collapse = (text) => text.replace(/\s+/g, ' ').trim();
describe('carrick#3741: an envelope read only for its status is not the response', () => {
    let client;
    let repoDir;
    let servicePath;
    before(async () => {
        repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3741-'));
        fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
        fs.writeFileSync(path.join(repoDir, 'tsconfig.json'), JSON.stringify({
            compilerOptions: {
                strict: true,
                module: 'esnext',
                moduleResolution: 'bundler',
                target: 'es2022',
                lib: ['es2022'],
                skipLibCheck: true,
            },
            include: ['src'],
        }));
        servicePath = path.join(repoDir, 'src', 'service.ts');
        fs.writeFileSync(servicePath, SERVICE_TS);
        client = new SidecarClient();
        await client.start();
        await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
    });
    after(async () => {
        await client.stop();
        fs.rmSync(repoDir, { recursive: true, force: true });
    });
    async function infer(alias, line, expressionText) {
        const res = await client.send({
            action: 'infer',
            request_id: alias,
            requests: [
                {
                    file_path: servicePath,
                    line_number: line,
                    infer_kind: 'call_result',
                    alias,
                    expression_text: expressionText,
                    expression_line: line,
                },
            ],
        });
        return (res.inferred_types ?? []).find((t) => t.alias === alias);
    }
    const rootReasons = (inferred) => (inferred.any_provenance ?? []).filter((p) => p.path === '').map((p) => p.reason);
    it('abstains where the site reads only the envelope status and error', async () => {
        const inferred = await infer('Endpoint_Close_Response_Status', STATUS_ONLY_LINE, 'closeTicket(id)');
        assert.ok(inferred, 'the row must be answered, with a decision on it');
        assert.ok(!/success/.test(inferred.type_string), `the envelope is not the wire body, got: ${inferred.type_string}`);
        assert.strictEqual(collapse(inferred.type_string), 'unknown');
        assert.strictEqual(inferred.primary_type_symbol, undefined);
        assert.strictEqual(inferred.array_depth, undefined);
        assert.deepStrictEqual(rootReasons(inferred), ['projected_value_only']);
    });
    it('keeps the answer it gave where the site reads the payload member', async () => {
        const inferred = await infer('Endpoint_Close_Response_Payload', PAYLOAD_READ_LINE, 'closeTicket(id)');
        assert.ok(inferred, 'the row must be answered');
        // carrick#1375 already decides this site: `res.data` resolves to the
        // envelope's type argument, so the site unwraps the envelope by hand.
        assert.strictEqual(collapse(inferred.type_string), 'unknown');
        assert.deepStrictEqual(rootReasons(inferred), ['projected_value_only']);
    });
    it('abstains on a multi-line call with a callback and a trailing comma', async () => {
        // The locator a scan sends for a call written over several lines: the
        // model's one-line expression text, at the line the call starts on.
        const inferred = await infer('Endpoint_Publish_Response_Status', MULTI_LINE_CALL_LINE, 'publishBuild(id, { tag }, (line) => { console.log(line); })');
        assert.ok(inferred, 'the row must be answered, with a decision on it');
        assert.strictEqual(collapse(inferred.type_string), 'unknown');
        assert.deepStrictEqual(rootReasons(inferred), ['projected_value_only']);
    });
    it('answers the declared result where the envelope carries no type argument', async () => {
        const inferred = await infer('Endpoint_Confirm_Response', NON_GENERIC_LINE, 'sendConfirmation(address)');
        assert.ok(inferred, 'the declared result is the contract here');
        assert.notStrictEqual(collapse(inferred.type_string), 'unknown');
        assert.match(inferred.type_string, /ok/);
    });
});
