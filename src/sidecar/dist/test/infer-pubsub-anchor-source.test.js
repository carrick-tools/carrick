/**
 * carrick#413 (two-anchor arbitration): the pub/sub infer kinds
 * (`function_param` for subscribers, `expression` for publishers) must report
 * the deterministic anchor of the resolved payload — `primary_type_symbol`
 * plus its declaration file (`primary_type_symbol_source`) — so the scanner
 * can compare the tsc-resolved root against an LLM-emitted explicit symbol
 * and, on a witnessed borrow, re-aim the explicit bundle request at the real
 * payload type. The type STRING keeps ts-morph's default named form; the
 * anchor is what makes the demotion self-contained.
 *
 * Anonymous payloads (destructured wrapper generics) stay anchor-less: no
 * root symbol means the arbitration never fires for them.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const FIXTURE = path.join(FIXTURES_PATH, 'src/pubsub-handlers.ts');
describe('pub/sub infer kinds report the payload anchor and its declaration source (carrick#413)', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'pas-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('function_param with a named annotation reports the root symbol and its source', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'pas-1',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: 89,
                    infer_kind: 'function_param',
                    param_name: 'evt',
                    alias: 'PubsubSubAnchor',
                },
            ],
        });
        assert.ok(response.inferred_types && response.inferred_types.length > 0, `expected inferred_types, got ${JSON.stringify(response)}`);
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.type_string, 'OrderPlacedPayload');
        assert.strictEqual(inferred.is_explicit, true);
        assert.strictEqual(inferred.primary_type_symbol, 'OrderPlacedPayload');
        assert.ok(inferred.primary_type_symbol_source?.endsWith('pubsub-handlers.ts'), `anchor source must be the declaration file, got ${JSON.stringify(inferred.primary_type_symbol_source)}`);
    });
    it('expression of a named publisher argument reports the root symbol and its source', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'pas-2',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: 95,
                    expression_text: 'order',
                    expression_line: 95,
                    infer_kind: 'expression',
                    alias: 'PubsubPubAnchor',
                },
            ],
        });
        assert.ok(response.inferred_types && response.inferred_types.length > 0, `expected inferred_types, got ${JSON.stringify(response)}`);
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.type_string, 'OrderPlacedPayload');
        assert.strictEqual(inferred.primary_type_symbol, 'OrderPlacedPayload');
        assert.ok(inferred.primary_type_symbol_source?.endsWith('pubsub-handlers.ts'), `anchor source must be the declaration file, got ${JSON.stringify(inferred.primary_type_symbol_source)}`);
    });
    it('a barrel-re-exported payload reports the DECLARING file, never the barrel', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'pas-4',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: 102,
                    infer_kind: 'function_param',
                    param_name: 'evt',
                    alias: 'PubsubBarrelAnchor',
                },
            ],
        });
        assert.ok(response.inferred_types && response.inferred_types.length > 0, `expected inferred_types, got ${JSON.stringify(response)}`);
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.primary_type_symbol, 'ReExportedPayload');
        assert.ok(inferred.primary_type_symbol_source?.endsWith('pubsub-barrel-types.ts'), `anchor source must be the declaring file, got ${JSON.stringify(inferred.primary_type_symbol_source)}`);
    });
    it('an anonymous destructured payload stays anchor-less', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'pas-3',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: 26,
                    infer_kind: 'function_param',
                    param_name: '{ time, item }',
                    alias: 'PubsubAnonAnchor',
                },
            ],
        });
        assert.ok(response.inferred_types && response.inferred_types.length > 0, `expected inferred_types, got ${JSON.stringify(response)}`);
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.primary_type_symbol, undefined);
        assert.strictEqual(inferred.primary_type_symbol_source, undefined);
    });
});
