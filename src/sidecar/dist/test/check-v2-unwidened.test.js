/**
 * carrick#1538: a pair is judged again with the producer's unwidened response
 * type when its published type mismatches.
 *
 * A handler that returns `cond ? 'a' : 'b'` publishes `string`, because a
 * literal widens where it is inferred. A consumer that DECLARES `'a' | 'b'`
 * then mismatches a response it reads correctly. The retype check clears that
 * for a consumer whose type was inferred (carrick#1516). A consumer that
 * states its type is never retyped, so the check itself reads the unwidened
 * alias the producer's stub ships (`<alias>_Unwidened`).
 *
 * `producer_wider` is filed only where the published assignment mismatches and
 * the second reading raises nothing. Every other outcome is the published
 * verdict, unchanged: a reading that still mismatches (a different literal, or
 * a member the handler may leave `undefined`), a reading that is or holds a
 * top type, a pair with no reading, a pair that agreed already, and a GraphQL
 * pair.
 *
 * carrick#2011: a request body is read again the same way, where the consumer
 * sends. A consumer has no class of its own on the wire: the pair it clears is
 * `compatible`, with a note that the published request is wider.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub, runCheck } from '../src/capture/index.js';
import { buildProbe, decisiveAssignmentLine, unwidenedAliasOf, } from '../src/capture/check-probe.js';
import { classifyPair, CONSUMER_WIDER_NOTE, } from '../src/capture/check-classify.js';
function captureLiterals(root, serviceName, literals) {
    const repoRoot = path.join(root, `${serviceName}-repo`);
    fs.mkdirSync(repoRoot, { recursive: true });
    const result = captureStub({
        repoRoot,
        serviceName,
        outDir: path.join(root, `${serviceName}-stub`),
        anchors: Object.entries(literals).map(([alias, text]) => ({
            kind: 'literal',
            alias,
            type_text: text,
            anchor_origin: 'deterministic-infer',
        })),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    return result;
}
/** A published response alias and, when given, the unwidened reading beside it. */
function response(alias, published, unwidened) {
    return unwidened === undefined
        ? { [alias]: published }
        : { [alias]: published, [unwidenedAliasOf(alias)]: unwidened };
}
const SCOPE_UNION = `{ id: string; scope: 'all' | 'specific' }`;
const SESSION_UNION = `{ active: true; session: { id: string } } | { active: false; session: null }`;
describe('check phase: a published mismatch is read again unwidened (carrick#1538)', () => {
    let root;
    let verdicts;
    let control;
    const pair = (pair_key, producerAlias, consumerAlias, over = {}) => ({
        pair_key,
        protocol: 'http',
        type_kind: 'response',
        producer: { service_name: 'wide-producer', alias: producerAlias },
        consumer: { service_name: 'wide-consumer', alias: consumerAlias },
        ...over,
    });
    const published = {
        ...response('P_Scope', `{ id: string; scope: string }`, SCOPE_UNION),
        ...response('P_NoReading', `{ id: string; scope: string }`),
        ...response('P_OtherLiteral', `{ id: string; scope: string }`, `{ id: string; scope: 'all' | 'other' }`),
        // The headline is a widened literal, and under it the handler may leave a
        // member out that the client requires.
        ...response('P_MaybeMember', `{ version: string; spec: { models: { name: string } | undefined } }`, `{ version: 'v1'; spec: { models: { name: string } | undefined } }`),
        // The same with the member optional, as an inferred return prints it.
        ...response('P_OptionalMember', `{ version: string; spec: { models?: { [key: string]: string } } }`, `{ version: 'v1'; spec: { models?: { [key: string]: string } } }`),
        ...response('P_Agrees', `{ id: string; scope: string }`, SCOPE_UNION),
        ...response('P_Session', `{ active: boolean; session: { id: string } | null }`, SESSION_UNION),
        ...response('P_Rank', `{ rank: number; at: Date }`, `{ rank: -1 | 2; at: Date }`),
        ...response('P_ReadingAny', `{ id: string; scope: string }`, `any`),
        ...response('P_ReadingDeepAny', `{ id: string; scope: string; meta: { tag: string } }`, `{ id: string; scope: 'all' | 'specific'; meta: any }`),
        ...response('P_Gql', `{ id: string; scope: string }`, SCOPE_UNION),
        P_Request: SCOPE_UNION,
        P_RequestNoReading: SCOPE_UNION,
        P_RequestOther: SCOPE_UNION,
    };
    const consumed = {
        C_Scope: SCOPE_UNION,
        C_NoReading: SCOPE_UNION,
        C_OtherLiteral: SCOPE_UNION,
        C_MaybeMember: `{ version: 'v1'; spec: { models: { name: string } } }`,
        C_OptionalMember: `{ version: 'v1'; spec: { models: { [key: string]: string } } }`,
        C_Agrees: `{ id: string; scope: string }`,
        C_Session: SESSION_UNION,
        C_Rank: `{ rank: -1 | 2; at: string }`,
        C_ReadingAny: SCOPE_UNION,
        C_ReadingDeepAny: `{ id: string; scope: 'all' | 'specific'; meta: { tag: string } }`,
        C_Gql: SCOPE_UNION,
        // A request body the consumer sends widened, with its reading beside it
        // (carrick#2011): the sender is the consumer.
        ...response('C_Request', `{ id: string; scope: string }`, `{ id: string; scope: 'all' }`),
        // A consumer that declares `string`: no reading.
        ...response('C_RequestNoReading', `{ id: string; scope: string }`),
        ...response('C_RequestOther', `{ id: string; scope: string }`, `{ id: string; scope: 'other' }`),
    };
    const pairs = [
        pair('scope', 'P_Scope', 'C_Scope'),
        pair('no-reading', 'P_NoReading', 'C_NoReading'),
        pair('other-literal', 'P_OtherLiteral', 'C_OtherLiteral'),
        pair('maybe-member', 'P_MaybeMember', 'C_MaybeMember'),
        pair('optional-member', 'P_OptionalMember', 'C_OptionalMember'),
        pair('agrees', 'P_Agrees', 'C_Agrees'),
        pair('session', 'P_Session', 'C_Session'),
        pair('rank', 'P_Rank', 'C_Rank'),
        pair('reading-any', 'P_ReadingAny', 'C_ReadingAny'),
        pair('reading-deep-any', 'P_ReadingDeepAny', 'C_ReadingDeepAny'),
        pair('gql', 'P_Gql', 'C_Gql', { protocol: 'graphql' }),
        pair('request', 'P_Request', 'C_Request', { type_kind: 'request' }),
        pair('request-no-reading', 'P_RequestNoReading', 'C_RequestNoReading', { type_kind: 'request' }),
        pair('request-other', 'P_RequestOther', 'C_RequestOther', { type_kind: 'request' }),
    ];
    /** The same types with every `_Unwidened` export left out: today's check. */
    const withoutReadings = (literals) => Object.fromEntries(Object.entries(literals).filter(([alias]) => !alias.endsWith('_Unwidened')));
    const check = async (dir, strip) => {
        const at = path.join(root, dir);
        const producer = captureLiterals(at, 'wide-producer', strip ? withoutReadings(published) : published);
        const consumer = captureLiterals(at, 'wide-consumer', strip ? withoutReadings(consumed) : consumed);
        const result = await runCheck({
            stubs: [
                { service_name: 'wide-producer', stub_dir: producer.stub_dir },
                { service_name: 'wide-consumer', stub_dir: consumer.stub_dir },
            ],
            pairs,
        });
        assert.strictEqual(result.success, true, JSON.stringify(result.errors));
        return new Map(result.verdicts.map((v) => [v.pair_key, v]));
    };
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1538-check-'));
        [verdicts, control] = await Promise.all([check('with', false), check('without', true)]);
    });
    after(() => {
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('the control (no reading shipped) calls every widened pair incompatible', () => {
        const widened = ['scope', 'session', 'rank', 'maybe-member', 'optional-member', 'other-literal'];
        for (const key of widened) {
            assert.strictEqual(control.get(key).bucket, 'incompatible', key);
        }
    });
    it('a widened literal union the consumer declares reads producer_wider', () => {
        const v = verdicts.get('scope');
        assert.strictEqual(v.bucket, 'producer_wider', JSON.stringify(v));
        assert.strictEqual(v.diagnostic, undefined, 'a reason is read as a mismatch');
        assert.strictEqual(v.gate, undefined);
        assert.strictEqual(v.resolved, true);
        assert.deepStrictEqual(v.codes, control.get('scope').codes);
    });
    it('a widened boolean discriminant reads producer_wider', () => {
        const v = verdicts.get('session');
        assert.strictEqual(v.bucket, 'producer_wider', JSON.stringify(v));
        assert.strictEqual(v.diagnostic, undefined);
    });
    it('the reading is judged in the wire form, like the published type', () => {
        // `at: Date` travels as a string, and only `rank` was widened.
        const v = verdicts.get('rank');
        assert.strictEqual(v.bucket, 'producer_wider', JSON.stringify(v));
    });
    it('a pair with no reading is exactly what it was', () => {
        assert.strictEqual(verdicts.get('no-reading').bucket, 'incompatible');
        assert.deepStrictEqual(verdicts.get('no-reading'), control.get('no-reading'));
    });
    it('a reading with a different literal leaves the published verdict', () => {
        const v = verdicts.get('other-literal');
        assert.strictEqual(v.bucket, 'incompatible', JSON.stringify(v));
        assert.deepStrictEqual(v, control.get('other-literal'));
        assert.doesNotMatch(v.diagnostic, /other/, 'the text is the published line');
    });
    it('a member the handler may leave undefined keeps the pair incompatible', () => {
        for (const key of ['maybe-member', 'optional-member']) {
            const v = verdicts.get(key);
            assert.strictEqual(v.bucket, 'incompatible', `${key}: ${JSON.stringify(v)}`);
            assert.deepStrictEqual(v, control.get(key), key);
        }
    });
    it('a pair that agreed as published is unchanged by its reading', () => {
        const v = verdicts.get('agrees');
        assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
        assert.deepStrictEqual(v, control.get('agrees'));
    });
    it('a reading that is a top type, or holds one, clears nothing', () => {
        for (const key of ['reading-any', 'reading-deep-any']) {
            const v = verdicts.get(key);
            assert.strictEqual(v.bucket, 'incompatible', `${key}: ${JSON.stringify(v)}`);
            assert.deepStrictEqual(v, control.get(key), key);
        }
    });
    it('a GraphQL pair gets no second reading', () => {
        assert.strictEqual(verdicts.get('gql').bucket, 'incompatible');
        assert.deepStrictEqual(verdicts.get('gql'), control.get('gql'));
    });
    it('a request body the consumer sends widened reads compatible, with a note (carrick#2011)', () => {
        assert.strictEqual(control.get('request').bucket, 'incompatible');
        const v = verdicts.get('request');
        assert.strictEqual(v.bucket, 'compatible', JSON.stringify(v));
        assert.strictEqual(v.diagnostic, undefined);
        assert.strictEqual(v.resolved, true);
        assert.ok(v.notes?.includes(CONSUMER_WIDER_NOTE), JSON.stringify(v.notes));
        assert.deepStrictEqual(v.codes, control.get('request').codes);
    });
    it('a request body the consumer declares, or sends another literal in, stays incompatible', () => {
        for (const key of ['request-no-reading', 'request-other']) {
            const v = verdicts.get(key);
            assert.strictEqual(v.bucket, 'incompatible', `${key}: ${JSON.stringify(v)}`);
            assert.deepStrictEqual(v, control.get(key), key);
        }
    });
});
describe('the unwidened reading in the probe and the classifier (carrick#1538)', () => {
    const PKG = (s) => `@carrick/${s}`;
    const spec = (over = {}) => ({
        pair_key: 'k1',
        protocol: 'http',
        type_kind: 'response',
        producer: { service_name: 'orders', alias: 'Endpoint_a_Response' },
        consumer: { service_name: 'web', alias: 'Call_a_Response' },
        ...over,
    });
    const reading = (endpoint) => unwidenedAliasOf(endpoint.alias);
    const scrubCtx = { workspaceRoot: '/tmp/ws', packageLabelOf: () => undefined };
    const noPoison = () => undefined;
    it('the reading is appended: no line of the probe moves', () => {
        const plain = buildProbe(spec(), PKG);
        const withReading = buildProbe(spec(), PKG, reading);
        assert.strictEqual(plain.unwidenedFromLine, undefined);
        assert.ok(withReading.source.startsWith(plain.source));
        assert.strictEqual(withReading.unwidenedFromLine, plain.source.split('\n').length);
        assert.deepStrictEqual(withReading.importLines, plain.importLines);
        assert.deepStrictEqual([...withReading.gateLines], [...plain.gateLines]);
        assert.strictEqual(withReading.pairId, plain.pairId);
        assert.match(withReading.source, /import type \{ Endpoint_a_Response_Unwidened as SentUnwidened \} from '@carrick\/orders';/);
    });
    it('a stub that ships no reading leaves the probe byte-identical', () => {
        assert.deepStrictEqual(buildProbe(spec(), PKG, () => undefined), buildProbe(spec(), PKG));
    });
    it('an http request reads the consumer, the side that sends it (carrick#2011)', () => {
        const plan = buildProbe(spec({ type_kind: 'request' }), PKG, reading);
        assert.ok(plan.unwidenedFromLine !== undefined);
        assert.match(plan.source, /import type \{ Call_a_Response_Unwidened as SentUnwidened \} from '@carrick\/web';/);
    });
    it('only an http pair is read again', () => {
        const others = [
            { protocol: 'graphql' },
            { protocol: 'socket', type_kind: 'both' },
            { protocol: 'pubsub', type_kind: 'both' },
        ];
        for (const over of others) {
            assert.deepStrictEqual(buildProbe(spec(over), PKG, reading), buildProbe(spec(over), PKG), JSON.stringify(over));
        }
    });
    describe('classifier', () => {
        const plan = buildProbe(spec(), PKG, reading);
        const from = plan.unwidenedFromLine;
        const diag = (line, code, message = 'x') => ({
            file: `packages/carrick-probes/probes/${plan.fileName}`,
            line,
            col: 1,
            code,
            message,
        });
        const mismatch = diag(decisiveAssignmentLine(plan), 2322, "Type 'string' is not assignable to type '\"all\" | \"specific\"'.");
        const clean = { sent: [], expected: [], sentUnwidened: [] };
        it('a published mismatch the reading does not repeat is producer_wider', () => {
            const v = classifyPair({
                plan,
                probeDiags: [mismatch],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: clean,
            });
            assert.strictEqual(v.bucket, 'producer_wider');
            assert.strictEqual(v.diagnostic, undefined);
            assert.strictEqual(v.resolved, true);
        });
        it('any diagnostic on the reading lines leaves the published verdict', () => {
            // A missing export on the import, a top-type gate, the assignment.
            for (const [line, code] of [
                [from, 2305],
                [from + 1, 2344],
                [from + 6, 2322],
            ]) {
                const v = classifyPair({
                    plan,
                    probeDiags: [mismatch, diag(line, code)],
                    poisonReason: noPoison,
                    scrubCtx,
                    deepFindings: clean,
                });
                assert.strictEqual(v.bucket, 'incompatible', `line ${line}`);
                assert.match(v.diagnostic, /Type 'string' is not assignable/);
                assert.deepStrictEqual(v.codes, [2322], 'the reading lines add no code');
            }
        });
        it('a diagnostic on the reading lines alone is no finding', () => {
            const v = classifyPair({
                plan,
                probeDiags: [diag(from, 2305), diag(from + 6, 2322)],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: clean,
            });
            assert.strictEqual(v.bucket, 'compatible');
            assert.deepStrictEqual(v.codes, []);
            assert.strictEqual(v.diagnostic, undefined);
        });
        it('a reading the member walk could not read, or found a top type in, clears nothing', () => {
            const unwalked = classifyPair({
                plan,
                probeDiags: [mismatch],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: { sent: [], expected: [] },
            });
            assert.strictEqual(unwalked.bucket, 'incompatible');
            const noWalk = classifyPair({ plan, probeDiags: [mismatch], poisonReason: noPoison, scrubCtx });
            assert.strictEqual(noWalk.bucket, 'incompatible');
            const deepAny = classifyPair({
                plan,
                probeDiags: [mismatch],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: {
                    sent: [],
                    expected: [],
                    sentUnwidened: [{ kind: 'any', path: 'meta', reason: 'declared' }],
                },
            });
            assert.strictEqual(deepAny.bucket, 'incompatible');
        });
        it('a mismatch that is not a fact stays the mismatch it was', () => {
            // The consumer holds a top type at its root (no named member is open),
            // so the reading fitting it proves nothing.
            const v = classifyPair({
                plan,
                probeDiags: [mismatch],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: {
                    sent: [],
                    expected: [{ kind: 'unknown', path: '', reason: 'declared' }],
                    sentUnwidened: [],
                },
            });
            assert.strictEqual(v.bucket, 'incompatible');
            assert.strictEqual(v.resolved, false);
            assert.strictEqual(v.unresolved_side, 'consumer');
        });
        it('a declared open unknown member on the receiving side is open like an any one (A2-1)', () => {
            const v = classifyPair({
                plan,
                probeDiags: [mismatch],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: {
                    sent: [],
                    expected: [{ kind: 'unknown', path: 'meta', reason: 'declared' }],
                    sentUnwidened: [],
                },
            });
            assert.strictEqual(v.bucket, 'incompatible');
            assert.strictEqual(v.resolved, true);
        });
        it('a declared open any member on a published side does not let the reading clear a mismatch (carrick#2591)', () => {
            const v = classifyPair({
                plan,
                probeDiags: [mismatch],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: {
                    sent: [],
                    expected: [{ kind: 'any', path: 'meta', reason: 'declared' }],
                    sentUnwidened: [],
                },
            });
            // The reading is not trusted against a member that accepts anything,
            // so the mismatch stands; it is a fact about the closed members.
            assert.strictEqual(v.bucket, 'incompatible');
            assert.strictEqual(v.resolved, true);
        });
        it('a reading whose own declarations do not typecheck clears nothing', () => {
            const v = classifyPair({
                plan,
                probeDiags: [mismatch],
                // Only the reading's alias is poisoned: the published pair is judged.
                poisonReason: (_service, alias) => alias === 'Endpoint_a_Response_Unwidened' ? 'poisoned' : undefined,
                scrubCtx,
                deepFindings: clean,
            });
            assert.strictEqual(v.bucket, 'incompatible');
        });
        it('a gate on the published type still outranks the reading', () => {
            const anyGateLine = [...plan.gateLines].find(([, name]) => name === 'sent:any')[0];
            const v = classifyPair({
                plan,
                probeDiags: [diag(anyGateLine, 2344), mismatch],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: clean,
            });
            assert.strictEqual(v.bucket, 'gate_caught_baked_any');
        });
        it('a sent union the consumer takes in part stays unverifiable', () => {
            const v = classifyPair({
                plan,
                probeDiags: [mismatch],
                poisonReason: noPoison,
                scrubCtx,
                deepFindings: clean,
                dispatchUnion: { members: 2, agreeing: 1 },
            });
            assert.strictEqual(v.bucket, 'unverifiable');
        });
    });
});
