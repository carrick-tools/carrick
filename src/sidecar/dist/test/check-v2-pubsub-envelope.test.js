/**
 * v2 check — pub/sub envelope gate (carrick#1979; real vendored pnpm + tsc).
 *
 * A subscriber's handler parameter is typed as the WHOLE message: the routing
 * key naming the topic, a variant tag, the payload and the fields the wrapper
 * stamps. A publisher is typed as the payload it passes in, or as a batch of
 * messages. Compared raw, every stamped field reads as missing. The probe
 * tests each side for a required member typed as this operation's topic
 * literal, and when exactly one side has it the pair is not compared.
 *
 * Pins the abstain AND its edges: both whole or neither whole is judged as
 * before, the gate keys on this operation's topic, it is pub/sub only, and a
 * side the decay gates decide gains no code from it.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck } from '../src/capture/index.js';
let root;
function writeStub(dir, serviceName, surface) {
    const stubDir = path.join(dir, serviceName);
    fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
    fs.writeFileSync(path.join(stubDir, 'package.json'), JSON.stringify({
        name: `@carrick/${serviceName}`,
        version: '0.0.0-carrick',
        private: true,
        types: './types/surface.d.ts',
    }, null, 2) + '\n');
    fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
}
// pub/sub: the subscriber is the pair's `producer`, the publisher its `consumer`.
function ps(pair_key, subscriberAlias, publisherAlias, over = {}) {
    return {
        pair_key,
        protocol: 'pubsub',
        type_kind: 'both',
        topic: 'orders',
        producer: { service_name: 'worker', alias: subscriberAlias },
        consumer: { service_name: 'publisher', alias: publisherAlias },
        ...over,
    };
}
const SUBSCRIBER_SURFACE = [
    'type Stamped = { id: string; sentAt: string };',
    'export type OrderEvent =',
    '  | (Stamped & { subject: "orders"; type: "orders.created"; payload: { orderId: string; total: number } })',
    '  | (Stamped & { subject: "orders"; type: "orders.cancelled"; payload: { orderId: string; reason: string } });',
    'export type Sub_Payload = { orderId: string; total: number };',
    'export type Sub_Unknown = unknown;',
].join('\n');
const PUBLISHER_SURFACE = [
    'type Stamped = { id: string; sentAt: string };',
    'export type OrderEvent =',
    '  | (Stamped & { subject: "orders"; type: "orders.created"; payload: { orderId: string; total: number } })',
    '  | (Stamped & { subject: "orders"; type: "orders.cancelled"; payload: { orderId: string; reason: string } });',
    'type Unstamped<T> = T extends unknown ? Omit<T, keyof Stamped> : never;',
    'export type Pub_Payload = { orderId: string; total: number };',
    'export type Pub_PayloadBad = { orderId: number; total: number };',
    'export type Pub_Batch = Unstamped<OrderEvent>[];',
    'export type Pub_Whole = OrderEvent;',
    'export type Pub_Optional = { subject?: "orders"; orderId: string; total: number };',
].join('\n');
const SUBSCRIBER_REASON = "the subscriber's type is the whole message and the publisher's type is not, so the two are not compared";
const PUBLISHER_REASON = "the publisher's type is the whole message and the subscriber's type is not, so the two are not compared";
const PAIRS = [
    ps('whole-vs-payload', 'OrderEvent', 'Pub_Payload'),
    ps('whole-vs-batch', 'OrderEvent', 'Pub_Batch'),
    ps('whole-vs-whole', 'OrderEvent', 'Pub_Whole'),
    ps('payload-vs-whole', 'Sub_Payload', 'Pub_Whole'),
    ps('payload-vs-payload', 'Sub_Payload', 'Pub_Payload'),
    ps('payload-vs-bad-payload', 'Sub_Payload', 'Pub_PayloadBad'),
    ps('other-topic', 'OrderEvent', 'Pub_Payload', { topic: 'invoices' }),
    ps('socket-not-gated', 'OrderEvent', 'Pub_Payload', { protocol: 'socket', topic: undefined }),
    ps('unknown-subscriber', 'Sub_Unknown', 'Pub_Whole'),
    ps('optional-topic-member', 'Sub_Payload', 'Pub_Optional'),
];
function byKey(verdicts) {
    return new Map(verdicts.map((v) => [v.pair_key, v]));
}
describe('check_v2 pub/sub envelope gate (real pnpm + tsc)', () => {
    let verdicts;
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-psenv-'));
        writeStub(root, 'worker', SUBSCRIBER_SURFACE);
        writeStub(root, 'publisher', PUBLISHER_SURFACE);
        const stubs = [
            { service_name: 'worker', stub_dir: path.join(root, 'worker') },
            { service_name: 'publisher', stub_dir: path.join(root, 'publisher') },
        ];
        const result = await runCheck({ stubs, pairs: PAIRS });
        assert.strictEqual(result.success, true, JSON.stringify(result.errors));
        assert.strictEqual(result.install_ok, true);
        verdicts = byKey(result.verdicts);
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    function abstains(key, gate, reason) {
        const v = verdicts.get(key);
        assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
        assert.strictEqual(v.gate, gate);
        assert.strictEqual(v.unresolved_reason, reason);
        assert.strictEqual(v.resolved, false);
        assert.strictEqual(v.unresolved_side, undefined);
    }
    it('the whole message against a payload -> unverifiable, subscriber is the whole', () => {
        abstains('whole-vs-payload', 'producer:envelope', SUBSCRIBER_REASON);
    });
    it('the whole message against a batch of messages -> unverifiable', () => {
        abstains('whole-vs-batch', 'producer:envelope', SUBSCRIBER_REASON);
    });
    it('both sides the whole message -> compared as before', () => {
        assert.strictEqual(verdicts.get('whole-vs-whole').bucket, 'compatible');
    });
    it('a payload against the whole message -> unverifiable, publisher is the whole', () => {
        abstains('payload-vs-whole', 'consumer:envelope', PUBLISHER_REASON);
    });
    it('neither side states the topic -> compared as before', () => {
        assert.strictEqual(verdicts.get('payload-vs-payload').bucket, 'compatible');
        assert.strictEqual(verdicts.get('payload-vs-bad-payload').bucket, 'incompatible');
    });
    it('the gate keys on this operation topic', () => {
        assert.strictEqual(verdicts.get('other-topic').bucket, 'incompatible');
    });
    it('the gate is pub/sub only', () => {
        assert.strictEqual(verdicts.get('socket-not-gated').bucket, 'incompatible');
    });
    it('an optional topic member does not state the topic', () => {
        assert.notStrictEqual(verdicts.get('optional-topic-member').gate, 'consumer:envelope');
        assert.notStrictEqual(verdicts.get('optional-topic-member').gate, 'producer:envelope');
    });
    it('an unknown subscriber stays with the unknown gate and carries no envelope code', () => {
        const v = verdicts.get('unknown-subscriber');
        assert.strictEqual(v.bucket, 'unverifiable');
        assert.strictEqual(v.gate, 'producer:unknown');
        assert.deepStrictEqual(v.codes, [2344]);
    });
});
