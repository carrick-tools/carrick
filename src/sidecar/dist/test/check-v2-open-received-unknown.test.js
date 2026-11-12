/**
 * A declared `unknown` under a named member of the RECEIVING side is an open
 * member, as a declared `any` already is (carrick#2591).
 *
 * Any type is assignable to `unknown`, so the compiler's assignment judges only
 * the closed members and no mismatch can hide in the open one. On the SENDING
 * side `unknown` stays a gate: a sent `unknown` is rejected by a narrower
 * receiver, so the pair is not a comparison.
 *
 * Pins:
 *  1. receiving side declares `unknown` under a member -> probed, compatible, a fact;
 *  2. a renamed closed member beside it reads incompatible;
 *  3. sending side declares `unknown` -> unverifiable, gated at capture;
 *  4. only a declared member qualifies: root, unresolved import, budget
 *     sentinel, callable return and a list at the walk's cap stay gated;
 *  5. the rule keys on direction, not on the producer/consumer label (a
 *     request body is received by the producer).
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import { buildProbe } from '../src/capture/check-probe.js';
import { classifyPair } from '../src/capture/check-classify.js';
import { MAX_DEEP_FINDINGS, onlyDeclaredOpenMembers } from '../src/capture/deep-walk.js';
function captureLiterals(root, serviceName, literals) {
    const repoRoot = path.join(root, `${serviceName}-repo`);
    fs.mkdirSync(repoRoot, { recursive: true });
    const result = captureStub({
        repoRoot,
        serviceName,
        outDir: path.join(root, `${serviceName}-stub`),
        anchors: Object.entries(literals).map(([alias, type_text]) => ({
            kind: 'literal',
            alias,
            type_text,
            anchor_origin: 'deterministic-infer',
        })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    return result;
}
describe('check phase: an unknown the receiving side declares is an open member', () => {
    let root;
    let producerStub;
    let consumerStub;
    before(() => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-open-received-unknown-'));
        // Invented shapes: a route's response and the client's copy of it.
        const producer = captureLiterals(root, 'shop-api', {
            P_Order: '{ order: { id: string; legacyCode: string }; audit: { by: string } }',
            P_OrderRenamed: '{ order: { id: string; code: string }; audit: { by: string } }',
            P_SendsUnknown: '{ order: { id: string; legacyCode: string }; audit: { [k: string]: unknown } }',
            // A request body is received by the producer.
            P_BodyReceives: '{ id: string; extra: { [k: string]: unknown } }',
        });
        const consumer = captureLiterals(root, 'shop-client', {
            C_Order: '{ order: { id: string; legacyCode: string }; audit?: { [k: string]: unknown } }',
            C_OrderRenamed: '{ order: { id: string; legacyCode: string }; audit?: { [k: string]: unknown } }',
            C_Narrow: '{ order: { id: string; legacyCode: string }; audit: { by: string } }',
            C_BodySends: '{ id: string; extra: { note: string } }',
        });
        producerStub = producer.stub_dir;
        consumerStub = consumer.stub_dir;
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('probes the pair and judges the closed members', async () => {
        const result = await runCheck({
            stubs: [
                { service_name: 'shop-api', stub_dir: producerStub },
                { service_name: 'shop-client', stub_dir: consumerStub },
            ],
            pairs: [
                {
                    pair_key: 'open-received',
                    protocol: 'http',
                    type_kind: 'response',
                    producer: { service_name: 'shop-api', alias: 'P_Order' },
                    consumer: { service_name: 'shop-client', alias: 'C_Order' },
                },
                {
                    pair_key: 'renamed-member',
                    protocol: 'http',
                    type_kind: 'response',
                    producer: { service_name: 'shop-api', alias: 'P_OrderRenamed' },
                    consumer: { service_name: 'shop-client', alias: 'C_OrderRenamed' },
                },
                {
                    pair_key: 'sends-unknown',
                    protocol: 'http',
                    type_kind: 'response',
                    producer: { service_name: 'shop-api', alias: 'P_SendsUnknown' },
                    consumer: { service_name: 'shop-client', alias: 'C_Narrow' },
                },
                {
                    pair_key: 'request-body',
                    protocol: 'http',
                    type_kind: 'request',
                    producer: { service_name: 'shop-api', alias: 'P_BodyReceives' },
                    consumer: { service_name: 'shop-client', alias: 'C_BodySends' },
                },
            ],
        });
        assert.strictEqual(result.success, true, JSON.stringify(result.errors));
        const byKey = new Map(result.verdicts.map((v) => [v.pair_key, v]));
        const open = byKey.get('open-received');
        assert.strictEqual(open.bucket, 'compatible', JSON.stringify(open));
        assert.strictEqual(open.resolved, true, JSON.stringify(open));
        const renamed = byKey.get('renamed-member');
        assert.strictEqual(renamed.bucket, 'incompatible', JSON.stringify(renamed));
        assert.match(renamed.diagnostic ?? '', /legacyCode/);
        // A sending-side unknown stays a gate (it rejects a narrower receiver).
        const sends = byKey.get('sends-unknown');
        assert.strictEqual(sends.bucket, 'unverifiable', JSON.stringify(sends));
        assert.strictEqual(sends.gate, 'capture:producer:unknown');
        // Request body: the producer receives, so ITS declared unknown is open, and
        // the consumer's concrete `extra` is assignable to it.
        const body = byKey.get('request-body');
        assert.strictEqual(body.bucket, 'compatible', JSON.stringify(body));
        assert.strictEqual(body.resolved, true, JSON.stringify(body));
    });
});
describe('what stays gated on the receiving side', () => {
    const declared = (path, kind = 'unknown') => ({
        path,
        kind,
        reason: 'declared',
    });
    it('a declared unknown under a named member is open only when receiving', () => {
        assert.strictEqual(onlyDeclaredOpenMembers([declared('audit[index]')], true), true);
        assert.strictEqual(onlyDeclaredOpenMembers([declared('audit[index]')], false), false);
        // A declared any is open on either side.
        assert.strictEqual(onlyDeclaredOpenMembers([declared('meta', 'any')], false), true);
        assert.strictEqual(onlyDeclaredOpenMembers([declared('meta', 'any')], true), true);
    });
    it('a root, a callable return, a bad reason, a budget sentinel and a capped list are not open', () => {
        const gated = [
            [declared('')],
            [declared('getData()')],
            [{ path: 'audit', kind: 'unknown', reason: 'unresolved_import' }],
            [{ path: 'audit', kind: 'budget_exhausted', reason: 'declared' }],
            Array.from({ length: MAX_DEEP_FINDINGS }, (_, i) => declared(`m${i}`)),
            // One open member does not vouch for a gated one beside it.
            [declared('audit'), declared('')],
        ];
        for (const findings of gated) {
            assert.strictEqual(onlyDeclaredOpenMembers(findings, true), false, JSON.stringify(findings));
        }
    });
    it('the classifier keeps a received unknown a fact and a sent one not-a-fact', () => {
        const spec = {
            pair_key: 'k',
            protocol: 'http',
            type_kind: 'response',
            producer: { service_name: 'a', alias: 'A' },
            consumer: { service_name: 'b', alias: 'B' },
        };
        const plan = buildProbe(spec, (s) => `@carrick/${s}`);
        const input = {
            plan,
            probeDiags: [],
            poisonReason: () => undefined,
            scrubCtx: { workspaceRoot: '/tmp/ws', packageLabelOf: () => undefined },
        };
        const received = classifyPair({
            ...input,
            deepFindings: { sent: [], expected: [declared('audit[index]')] },
        });
        assert.strictEqual(received.bucket, 'compatible');
        assert.strictEqual(received.resolved, true);
        const sent = classifyPair({
            ...input,
            deepFindings: { sent: [declared('audit[index]')], expected: [] },
        });
        assert.strictEqual(sent.bucket, 'compatible');
        assert.strictEqual(sent.resolved, false);
    });
});
describe('a sending-side unknown is open where the receiver declares a top type at the same path', () => {
    const declared = (path, kind = 'unknown') => ({
        path,
        kind,
        reason: 'declared',
    });
    it('matches by identical path, including index elements and nested members', () => {
        for (const p of ['audit', 'audit[index]', 'order.meta[index]', 'rows<0>.extra']) {
            assert.strictEqual(onlyDeclaredOpenMembers([declared(p)], false, [declared(p)]), true, p);
            assert.strictEqual(onlyDeclaredOpenMembers([declared(p)], false, [declared(p, 'any')]), true, p);
        }
    });
    it('stays gated against a concrete receiver, another path, or an unvouching list', () => {
        const sent = [declared('audit[index]')];
        assert.strictEqual(onlyDeclaredOpenMembers(sent, false, []), false);
        assert.strictEqual(onlyDeclaredOpenMembers(sent, false), false);
        assert.strictEqual(onlyDeclaredOpenMembers(sent, false, [declared('audit')]), false);
        assert.strictEqual(onlyDeclaredOpenMembers(sent, false, [declared('other[index]')]), false);
        // The receiver's own root top type or non-declared reason is no match.
        assert.strictEqual(onlyDeclaredOpenMembers(sent, false, [
            { path: 'audit[index]', kind: 'unknown', reason: 'unresolved_import' },
        ]), false);
        // A receiver list at the walk's cap may have stopped listing.
        const capped = Array.from({ length: MAX_DEEP_FINDINGS }, (_, i) => declared(`m${i}`));
        capped[0] = declared('audit[index]');
        assert.strictEqual(onlyDeclaredOpenMembers(sent, false, capped), false);
        // One matched member does not vouch for an unmatched one beside it.
        assert.strictEqual(onlyDeclaredOpenMembers([declared('audit'), declared('x')], false, [declared('audit')]), false);
    });
    it('the classifier calls the pair a fact only when both sides declare it', () => {
        const spec = {
            pair_key: 'k',
            protocol: 'http',
            type_kind: 'response',
            producer: { service_name: 'a', alias: 'A' },
            consumer: { service_name: 'b', alias: 'B' },
        };
        const plan = buildProbe(spec, (s) => `@carrick/${s}`);
        const input = {
            plan,
            probeDiags: [],
            poisonReason: () => undefined,
            scrubCtx: { workspaceRoot: '/tmp/ws', packageLabelOf: () => undefined },
        };
        const both = classifyPair({
            ...input,
            deepFindings: { sent: [declared('audit[index]')], expected: [declared('audit[index]')] },
        });
        assert.strictEqual(both.resolved, true);
        const concrete = classifyPair({
            ...input,
            deepFindings: { sent: [declared('audit[index]')], expected: [] },
        });
        assert.strictEqual(concrete.resolved, false);
    });
});
describe('check phase: sender and receiver both declare unknown at the same path', () => {
    let root;
    let producerStub;
    let consumerStub;
    before(() => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-open-sent-unknown-'));
        producerStub = captureLiterals(root, 'shop-api', {
            P_Both: '{ order: { id: string; legacyCode: string }; audit: { [k: string]: unknown } }',
            P_BothRenamed: '{ order: { id: string; code: string }; audit: { [k: string]: unknown } }',
            P_Concrete: '{ order: { id: string; legacyCode: string }; audit: { [k: string]: unknown } }',
        }).stub_dir;
        consumerStub = captureLiterals(root, 'shop-client', {
            C_Both: '{ order: { id: string; legacyCode: string }; audit?: { [k: string]: unknown } }',
            C_Concrete: '{ order: { id: string; legacyCode: string }; audit: { by: string } }',
        }).stub_dir;
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('judges the pair, and still fails a renamed closed member', async () => {
        const mk = (key, p, c) => ({
            pair_key: key,
            protocol: 'http',
            type_kind: 'response',
            producer: { service_name: 'shop-api', alias: p },
            consumer: { service_name: 'shop-client', alias: c },
        });
        const result = await runCheck({
            stubs: [
                { service_name: 'shop-api', stub_dir: producerStub },
                { service_name: 'shop-client', stub_dir: consumerStub },
            ],
            pairs: [
                mk('both', 'P_Both', 'C_Both'),
                mk('renamed', 'P_BothRenamed', 'C_Both'),
                mk('concrete', 'P_Concrete', 'C_Concrete'),
            ],
        });
        assert.strictEqual(result.success, true, JSON.stringify(result.errors));
        const byKey = new Map(result.verdicts.map((v) => [v.pair_key, v]));
        const both = byKey.get('both');
        assert.strictEqual(both.bucket, 'compatible', JSON.stringify(both));
        assert.strictEqual(both.resolved, true, JSON.stringify(both));
        assert.strictEqual(byKey.get('renamed').bucket, 'incompatible');
        const concrete = byKey.get('concrete');
        assert.strictEqual(concrete.bucket, 'unverifiable', JSON.stringify(concrete));
        assert.strictEqual(concrete.gate, 'capture:producer:unknown');
    });
});
