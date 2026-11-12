/**
 * The `invoke_client` role (carrick#3439, ticket S): a client that calls a
 * deployed function by its name, written as a command object built with
 * `new` and sent through a client's `send`. Its one op is `request`: the
 * function's name at a key of the constructor's argument, the payload at
 * another. It states no protocol value (`invoke` is not a claim protocol), it
 * never receives (the receive side is declared in `carrick.json`), and every
 * other role's checks are unchanged.
 *
 * The package below is made up; its input type has the shape that matters
 * here: the name key beside other optional string keys and a byte payload.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { LibraryClaimsVerifier } from '../src/library-claims.js';
import { parseRequest } from '../src/validators.js';
const TSCONFIG = JSON.stringify({
    compilerOptions: {
        target: 'es2020',
        module: 'commonjs',
        moduleResolution: 'node',
        lib: ['es2020'],
        strict: true,
        esModuleInterop: true,
        skipLibCheck: true,
        types: [],
    },
    include: ['src/**/*.ts'],
});
const FUNCTIONS = `export interface CallInput {
  FunctionName: string | undefined;
  InvocationType?: 'Event' | 'RequestResponse' | 'DryRun';
  Qualifier?: string;
  ClientContext?: string;
  Payload?: Uint8Array;
}
export interface CallOutput { StatusCode?: number; Payload?: Uint8Array }
export declare class CallCommand {
  readonly input: CallInput;
  constructor(input: CallInput);
}
export declare class FunctionsClient {
  constructor(config?: { region?: string });
  send(command: CallCommand): Promise<CallOutput>;
}
`;
const PKG = 'fixture-functions';
let sequence = 0;
const invoke = (claim, role = 'invoke_client') => ({ claim_id: `i${sequence++}`, package: PKG, export: 'CallCommand', role, receiver: 'export', claim });
/** The store's label for a direct invoke (the plan's R10). */
const REQUEST = {
    kind: 'op',
    op: 'request',
    member: null,
    on: 'export',
    form: 'new',
    name: { arg: 0, key: 'FunctionName' },
    payload: { arg: 0, key: 'Payload' },
    key_labels: { Qualifier: 'not_name', ClientContext: 'not_name' },
};
describe('verify_library_claims: invoke_client (carrick#3439)', () => {
    let root;
    let verifier;
    before(() => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-invoke-'));
        const files = {
            'tsconfig.json': TSCONFIG,
            'package.json': JSON.stringify({ name: 'svc', dependencies: { [PKG]: '1.0.0' } }),
            'src/index.ts': 'export const service = 1;\n',
            [`node_modules/${PKG}/package.json`]: JSON.stringify({ name: PKG, version: '1.0.0', types: 'index.d.ts' }),
            [`node_modules/${PKG}/index.d.ts`]: FUNCTIONS,
        };
        for (const [rel, text] of Object.entries(files)) {
            const file = path.join(root, rel);
            fs.mkdirSync(path.dirname(file), { recursive: true });
            fs.writeFileSync(file, text);
        }
        verifier = new LibraryClaimsVerifier(new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') }));
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    const verdict = (check) => {
        const [result] = verifier.run(root, [check], 60_000).semantics;
        return result.verdict === 'verified' ? 'verified' : `${result.verdict} ${result.reason}`;
    };
    it('verifies a request built with new, the name and the payload at keys of its one argument', () => {
        assert.strictEqual(verdict(invoke(REQUEST)), 'verified');
    });
    it('reads the request without a payload slot', () => {
        const { payload: _payload, ...nameOnly } = REQUEST;
        assert.strictEqual(verdict(invoke(nameOnly)), 'verified');
    });
    it('keeps strict D2: an optional string key left unlabelled competes with the name', () => {
        const { key_labels: _labels, ...unlabelled } = REQUEST;
        assert.strictEqual(verdict(invoke(unlabelled)), 'failed name_ambiguous');
    });
    it('refuses a name key the input type does not declare', () => {
        assert.notStrictEqual(verdict(invoke({ ...REQUEST, name: { arg: 0, key: 'FunctionArn' } })), 'verified');
    });
    it('checks only requests: a send or a receive is role_unsupported', () => {
        assert.strictEqual(verdict(invoke({ ...REQUEST, op: 'send' })), 'unchecked role_unsupported');
        const { payload: _payload, ...receive } = REQUEST;
        assert.strictEqual(verdict(invoke({ ...receive, op: 'receive' })), 'unchecked role_unsupported');
    });
    it('states no claim protocol: any protocol value is protocol_role', () => {
        for (const protocol of ['http', 'message', 'rpc']) {
            assert.strictEqual(verdict(invoke({ ...REQUEST, protocol })), 'unchecked protocol_role', protocol);
        }
    });
    it('accepts the role on the wire', () => {
        const parsed = parseRequest({
            action: 'verify_library_claims',
            request_id: 'r1',
            from_dir: root,
            checks: [invoke(REQUEST)],
        });
        assert.ok(parsed.success, JSON.stringify(parsed));
    });
    it('leaves the same claim under broker as it was', () => {
        assert.strictEqual(verdict(invoke({ ...REQUEST, op: 'send' }, 'broker')), 'verified');
    });
});
