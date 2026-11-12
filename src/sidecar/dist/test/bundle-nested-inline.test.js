/**
 * Regression: the explicit-symbol bundle path (`bundle` action →
 * TypeBundler.extractTypeDefinition) must inline NESTED named members, not just
 * the top-level shape.
 *
 * This is the path the GraphQL/Socket consumer anchors take (see
 * file_orchestrator.rs `collect_graphql_type_requests` /
 * `collect_socket_type_requests`): a `SymbolRequest` for the consumer's TS
 * result type is bundled, then `resolve_definitions` expands it. Socket payloads
 * have flat primitive fields, so the gap stayed hidden until a symbol with a
 * nested named member appeared (GraphQL `OrderView { total: MoneyView }`).
 *
 * The bundler used to emit the interface's verbatim source text, so a nested
 * named member (`settings: UserSettings`) stayed a bare reference. The bundle
 * carries only the alias line — never `UserSettings`'s declaration — so that
 * reference is dangling and resolves to `any` downstream, and the later
 * structural expand has nothing to inline against. The fix expands the body
 * structurally at bundle time, in the source project where the nested symbol
 * still resolves.
 *
 * UserProfile (test/fixtures/sample-repo/src/types.ts) mirrors the OrderView
 * shape: an `extends`-inherited base, a nested named member (`UserSettings`),
 * optional fields (`bio?`/`avatar?`), a builtin (`Date`) that must stay by name,
 * and a string-literal union.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import { SidecarClient, FIXTURES_PATH, stubDirFor } from './helpers.js';
const TYPES_FIXTURE = path.join(FIXTURES_PATH, 'src/types.ts');
const noWs = (s) => s.replace(/\s/g, '');
describe('Explicit-symbol bundle inlines nested named members', () => {
    let client;
    before(async () => {
        client = new SidecarClient();
        await client.start();
        await client.send({
            action: 'init',
            request_id: 'nested-init',
            repo_root: FIXTURES_PATH,
        });
    });
    after(async () => {
        await client.stop();
    });
    it('inlines a nested named member at bundle time (UserProfile.settings)', async () => {
        const ALIAS = 'Endpoint_abc123_Response';
        const bundle = await client.send({
            action: 'bundle',
            request_id: 'nested-bundle',
            symbols: [
                {
                    symbol_name: 'UserProfile',
                    source_file: TYPES_FIXTURE,
                    alias: ALIAS,
                },
            ],
        });
        assert.strictEqual(bundle.status, 'success', `bundle failed: ${JSON.stringify(bundle.errors)}`);
        const dts = bundle.dts_content ?? '';
        // The alias declaration is present under the requested name.
        assert.ok(dts.includes(`interface ${ALIAS}`), `expected the aliased interface, got:\n${dts}`);
        // The nested named member must be INLINED, not left as a bare reference.
        assert.ok(!/settings\s*:\s*UserSettings/.test(dts), `nested member must be inlined, found bare 'settings: UserSettings':\n${dts}`);
        assert.ok(noWs(dts).includes('settings:{') &&
            noWs(dts).includes('notifications:boolean'), `expected settings inlined to its structure, got:\n${dts}`);
        // Optional markers survive.
        assert.ok(noWs(dts).includes('bio?:'), `expected optional bio?, got:\n${dts}`);
        // Inherited members (extends User) are present.
        assert.ok(noWs(dts).includes('email:string'), `expected inherited email, got:\n${dts}`);
        // Builtins are NOT over-expanded: Date stays `Date`.
        assert.ok(noWs(dts).includes('createdAt:Date'), `Date must stay by name, got:\n${dts}`);
    });
    it('resolves the bundled alias to a fully-inlined structural string', async () => {
        const ALIAS = 'Endpoint_def456_Response';
        const bundle = await client.send({
            action: 'bundle',
            request_id: 'nested-bundle-2',
            symbols: [
                {
                    symbol_name: 'UserProfile',
                    source_file: TYPES_FIXTURE,
                    alias: ALIAS,
                },
            ],
        });
        assert.strictEqual(bundle.status, 'success');
        const resolved = await client.send({
            action: 'resolve_definitions',
            request_id: 'nested-resolve',
            stub_dir: stubDirFor(bundle.dts_content ?? ''),
            aliases: [ALIAS],
        });
        assert.strictEqual(resolved.status, 'success', `resolve failed: ${JSON.stringify(resolved.errors)}`);
        const def = resolved.definitions?.find((d) => d.type_alias === ALIAS);
        assert.ok(def, `expected a resolved definition for ${ALIAS}`);
        // The end-to-end expanded string (what reaches the manifest entry) must have
        // the nested member fully inlined — no dangling `UserSettings`.
        assert.ok(!def.expanded.includes('UserSettings'), `expanded must not contain dangling UserSettings, got: ${def.expanded}`);
        assert.ok(noWs(def.expanded).includes('settings:{') &&
            noWs(def.expanded).includes('notifications:boolean'), `expanded must inline settings structurally, got: ${def.expanded}`);
        assert.ok(noWs(def.expanded).includes('bio?:'), `expanded must preserve optional bio?, got: ${def.expanded}`);
    });
});
