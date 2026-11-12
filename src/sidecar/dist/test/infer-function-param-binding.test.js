/**
 * `function_param` locator resolution against destructured handler
 * parameters (the pub/sub wrapper-subscriber shapes):
 *
 * 1. whole binding pattern (`{ time, item }`) — the handler destructures the
 *    payload itself, so the pattern's type IS the payload type;
 * 2. binding element (`payload`) inside an envelope param — the checker
 *    projects the element's type out of the generic envelope.
 *
 * Both read types the checker has already instantiated from the wrapper's
 * generics; no named payload symbol exists anywhere in the fixture.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';
const FIXTURE = path.join(FIXTURES_PATH, 'src/pubsub-handlers.ts');
const noWs = (s) => s.replace(/\s/g, '');
describe('function_param resolves destructured pub/sub handler payloads', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'fpb-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('matches a whole binding pattern and returns the payload type', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'fpb-1',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: 26,
                    infer_kind: 'function_param',
                    param_name: '{ time, item }',
                    alias: 'FPB_WholePattern',
                },
            ],
        });
        const inferred = response.inferred_types?.[0];
        assert.ok(inferred, 'whole-pattern param should infer');
        assert.strictEqual(inferred.is_explicit, false);
        const t = noWs(inferred.type_string);
        assert.ok(t.includes('time:Date'), `missing time member: ${inferred.type_string}`);
        assert.ok(t.includes('id:string') && t.includes('status:string'), `missing item members: ${inferred.type_string}`);
    });
    it('projects a named binding element out of an envelope param', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'fpb-2',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: 45,
                    infer_kind: 'function_param',
                    param_name: 'payload',
                    alias: 'FPB_BindingElement',
                },
            ],
        });
        const inferred = response.inferred_types?.[0];
        assert.ok(inferred, 'binding-element locator should infer');
        const t = noWs(inferred.type_string);
        assert.ok(t.includes('resourceId:string'), `missing resourceId: ${inferred.type_string}`);
        assert.ok(t.includes('"full"|"partial"'), `missing mode union: ${inferred.type_string}`);
        assert.ok(!t.includes('id:string;payload'), `must project the element, not the whole envelope: ${inferred.type_string}`);
    });
    it('still fails cleanly for a name that matches nothing', async () => {
        const response = await client.send({
            action: 'infer',
            request_id: 'fpb-3',
            requests: [
                {
                    file_path: FIXTURE,
                    line_number: 26,
                    infer_kind: 'function_param',
                    param_name: 'nonexistent',
                    alias: 'FPB_Missing',
                },
            ],
        });
        assert.strictEqual(response.inferred_types?.length ?? 0, 0);
    });
});
