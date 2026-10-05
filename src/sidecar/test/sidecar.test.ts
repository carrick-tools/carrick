/**
 * Integration tests for the type-sidecar
 *
 * These tests spawn the sidecar process and communicate via stdin/stdout
 * to verify the full message loop functionality.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import { SidecarClient, FIXTURES_PATH, stubDirFor } from './helpers.js';

// ===========================================================================
// Tests
// ===========================================================================

describe('Type Sidecar Integration Tests', () => {
  let client: SidecarClient;

  before(async () => {
    client = new SidecarClient();
    await client.start();
  });

  after(async () => {
    await client.stop();
  });

  describe('init action', () => {
    it('should initialize with a valid repo root', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        init_time_ms?: number;
      }>({
        action: 'init',
        request_id: 'init-1',
        repo_root: FIXTURES_PATH,
      });

      assert.strictEqual(response.request_id, 'init-1');
      assert.strictEqual(response.status, 'ready');
      assert.ok(typeof response.init_time_ms === 'number');
      assert.ok(response.init_time_ms >= 0, 'init_time_ms should be non-negative');
    });

    it('should fail with non-existent repo root', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        errors?: string[];
      }>({
        action: 'init',
        request_id: 'init-2',
        repo_root: '/non/existent/path',
      });

      assert.strictEqual(response.request_id, 'init-2');
      assert.strictEqual(response.status, 'error');
      assert.ok(response.errors && response.errors.length > 0);
    });
  });

  describe('health action', () => {
    it('should report ready status after initialization', async () => {
      // Re-initialize to ensure we're in a good state
      await client.send({
        action: 'init',
        request_id: 'health-setup',
        repo_root: FIXTURES_PATH,
      });

      const response = await client.send<{
        request_id: string;
        status: string;
        init_time_ms?: number;
      }>({
        action: 'health',
        request_id: 'health-1',
      });

      assert.strictEqual(response.request_id, 'health-1');
      assert.strictEqual(response.status, 'ready');
      assert.ok(typeof response.init_time_ms === 'number');
    });
  });

  describe('bundle action', () => {
    before(async () => {
      // Ensure initialized
      await client.send({
        action: 'init',
        request_id: 'bundle-setup',
        repo_root: FIXTURES_PATH,
      });
    });

    it('should bundle a simple interface', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        dts_content?: string;
        manifest?: Array<{
          alias: string;
          original_name: string;
          source_file: string;
          is_explicit: boolean;
        }>;
        errors?: string[];
      }>({
        action: 'bundle',
        request_id: 'bundle-1',
        symbols: [
          {
            symbol_name: 'User',
            source_file: 'src/types.ts',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'bundle-1');
      assert.strictEqual(response.status, 'success');
      assert.ok(response.dts_content, 'Should have dts_content');
      assert.ok(response.dts_content.includes('User'), 'dts_content should include User');
      assert.ok(response.manifest, 'Should have manifest');
      assert.strictEqual(response.manifest.length, 1);
      assert.strictEqual(response.manifest[0].original_name, 'User');
      assert.strictEqual(response.manifest[0].is_explicit, true);
    });

    it('should bundle multiple symbols', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        dts_content?: string;
        manifest?: Array<{
          alias: string;
          original_name: string;
        }>;
      }>({
        action: 'bundle',
        request_id: 'bundle-2',
        symbols: [
          { symbol_name: 'User', source_file: 'src/types.ts' },
          { symbol_name: 'Order', source_file: 'src/models.ts' },
        ],
      });

      assert.strictEqual(response.request_id, 'bundle-2');
      assert.strictEqual(response.status, 'success');
      assert.ok(response.dts_content);
      assert.ok(response.dts_content.includes('User'));
      assert.ok(response.dts_content.includes('Order'));
      assert.strictEqual(response.manifest?.length, 2);
    });

    it('should support symbol aliases', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        manifest?: Array<{
          alias: string;
          original_name: string;
        }>;
      }>({
        action: 'bundle',
        request_id: 'bundle-3',
        symbols: [
          {
            symbol_name: 'User',
            source_file: 'src/types.ts',
            alias: 'UserResponse',
          },
        ],
      });

      assert.strictEqual(response.status, 'success');
      assert.ok(response.manifest);
      assert.strictEqual(response.manifest[0].alias, 'UserResponse');
      assert.strictEqual(response.manifest[0].original_name, 'User');
    });

    it('should report errors for non-existent symbols', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        symbol_failures?: Array<{
          symbol_name: string;
          reason: string;
        }>;
      }>({
        action: 'bundle',
        request_id: 'bundle-4',
        symbols: [
          {
            symbol_name: 'NonExistentType',
            source_file: 'src/types.ts',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'bundle-4');
      assert.strictEqual(response.status, 'error');
      assert.ok(response.symbol_failures);
      assert.strictEqual(response.symbol_failures.length, 1);
      assert.strictEqual(response.symbol_failures[0].symbol_name, 'NonExistentType');
    });
  });

  describe('infer action', () => {
    before(async () => {
      // Ensure initialized
      await client.send({
        action: 'init',
        request_id: 'infer-setup',
        repo_root: FIXTURES_PATH,
      });
    });

    it('should infer function return type', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
          source_location: {
            file_path: string;
            start_line: number;
            end_line: number;
          };
        }>;
        errors?: string[];
      }>({
        action: 'infer',
        request_id: 'infer-1',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 55, // getUser function (line inside the function body)
            span_start: 1537,
            span_end: 1541,
            infer_kind: 'function_return',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-1');
      assert.ok(
        response.status === 'success' || response.inferred_types,
        'Should succeed or have inferred types'
      );
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'function_return');
        assert.ok(inferred.source_location.file_path.includes('routes.ts'));
      }
    });

    it('should infer response body type from res.json()', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-2',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 87, // getOrders function body (implicit types)
            span_start: 1770,
            span_end: 1776,
            infer_kind: 'response_body',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-2');
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'response_body');
        // For implicit types, is_explicit should be false
        assert.strictEqual(inferred.is_explicit, false);
      }
    });

    it('should infer response body type from multiline expressions', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-2a',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/multiline.ts'),
            line_number: 6,
            span_start: 121,
            span_end: 224,
            infer_kind: 'response_body',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-2a');
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'response_body');
        assert.ok(inferred.type_string.includes('id'));
        assert.ok(inferred.type_string.includes('meta'));
      }
    });

    // Move 1: the LLM emits the payload subexpression directly (e.g., `users`),
    // not the surrounding call (`res.json(users)`). The sidecar resolves the
    // node by text and returns its type, working identically across
    // res.json(...), ctx.body = ..., h.response(...), and bare return.
    //
    // #257/#240: the resolved named payload is now expanded STRUCTURALLY (so the
    // cross-repo bundle carries the real members, not a dangling name). The
    // assertions check the inlined member shape rather than the bare type name.
    it('should resolve response payload via text locator — Express res.json(x)', async () => {
      const response = await client.send<{
        inferred_types?: Array<{ type_string: string; infer_kind: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-payload-express',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/framework-handlers.ts'),
            line_number: 25,
            expression_text: 'users',
            expression_line: 25,
            infer_kind: 'response_body',
          },
        ],
      });

      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'response_body');
        // Structural, not the dangling bare `User[]`.
        assert.strictEqual(
          inferred.type_string,
          '{ id: number; name: string; }[]',
          `expected structural User[], got ${inferred.type_string}`
        );
      } else {
        assert.fail('expected inferred_types for Express payload');
      }
    });

    it('should resolve response payload via text locator — Koa ctx.body = x', async () => {
      const response = await client.send<{
        inferred_types?: Array<{ type_string: string; infer_kind: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-payload-koa',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/framework-handlers.ts'),
            line_number: 31,
            expression_text: 'users',
            expression_line: 31,
            infer_kind: 'response_body',
          },
        ],
      });

      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        // Structural, not the dangling bare `User[]`.
        assert.strictEqual(
          inferred.type_string,
          '{ id: number; name: string; }[]',
          `expected structural User[], got ${inferred.type_string}`
        );
      } else {
        assert.fail('expected inferred_types for Koa payload');
      }
    });

    it('should resolve response payload via text locator — Hapi h.response(x)', async () => {
      const response = await client.send<{
        inferred_types?: Array<{ type_string: string; infer_kind: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-payload-hapi',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/framework-handlers.ts'),
            line_number: 37,
            expression_text: 'order',
            expression_line: 37,
            infer_kind: 'response_body',
          },
        ],
      });

      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        // Structural, not the dangling bare `Order`.
        assert.strictEqual(
          inferred.type_string,
          '{ id: number; total: number; }',
          `expected structural Order, got ${inferred.type_string}`
        );
      } else {
        assert.fail('expected inferred_types for Hapi payload');
      }
    });

    it('should resolve response payload via text locator — bare return (Fastify/NestJS/Hono)', async () => {
      const response = await client.send<{
        inferred_types?: Array<{ type_string: string; infer_kind: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-payload-bare-return',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/framework-handlers.ts'),
            line_number: 43,
            expression_text: 'user',
            expression_line: 43,
            infer_kind: 'response_body',
          },
        ],
      });

      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        // Structural, not the dangling bare `User`.
        assert.strictEqual(
          inferred.type_string,
          '{ id: number; name: string; }',
          `expected structural User, got ${inferred.type_string}`
        );
      } else {
        assert.fail('expected inferred_types for bare-return payload');
      }
    });

    it('should return a response_body inference when the locator resolves to a function identifier', async () => {
      // Canary for the "payload-less handler" path. The LLM sometimes emits a
      // function identifier (e.g., `bareReturnHandler`) as the locator; the
      // sidecar must resolve it and emit a non-empty response_body inference
      // rather than crashing or silently dropping the request.
      //
      // Known limitations (tracked separately from Copilot's 2026-04-20 review):
      //   - The sidecar currently returns `typeof bareReturnHandler` rather than
      //     unwrapping to the function's return type. That's a follow-up.
      //   - The "no locator at all" case (only file_path + line_number) is NOT
      //     exercised here; `inferFunctionReturn` needs an anchor to find the
      //     containing function.
      const response = await client.send<{
        inferred_types?: Array<{ type_string: string; infer_kind: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-payload-fallback',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/framework-handlers.ts'),
            line_number: 42,
            expression_text: 'bareReturnHandler',
            expression_line: 42,
            infer_kind: 'response_body',
          },
        ],
      });

      assert.ok(
        response.inferred_types && response.inferred_types.length > 0,
        `expected inferred_types to be non-empty, got ${JSON.stringify(response)}`
      );
      const inferred = response.inferred_types![0];
      assert.strictEqual(inferred.infer_kind, 'response_body');
      assert.ok(
        inferred.type_string && inferred.type_string.length > 0,
        `expected non-empty type_string, got ${JSON.stringify(inferred)}`
      );
    });

    it('should infer request body type from handler usage', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-2b',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/request-bodies.ts'),
            line_number: 14,
            span_start: 207,
            span_end: 215,
            infer_kind: 'request_body',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-2b');
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'request_body');
        // The body resolves to the named `RequestBody`, now expanded
        // structurally so the real members reach the cross-repo bundle instead
        // of a dangling bare name. Assert the members, not the name.
        assert.ok(
          inferred.type_string.includes('name') &&
            inferred.type_string.includes('email'),
          `expected the structural RequestBody shape, got ${inferred.type_string}`
        );
      }
    });

    it('should infer request body type from call payloads', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-2c',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/request-bodies.ts'),
            line_number: 20,
            span_start: 344,
            span_end: 348,
            infer_kind: 'request_body',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-2c');
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'request_body');
        // The serialized payload resolves to the named `RequestBody`, now
        // expanded structurally so the real members reach the cross-repo bundle
        // instead of a dangling bare name. Assert the members, not the name.
        assert.ok(
          inferred.type_string.includes('name') &&
            inferred.type_string.includes('email'),
          `expected the structural RequestBody shape, got ${inferred.type_string}`
        );
      }
    });

    it('should support custom aliases for inferred types', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-3',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 55, // Inside getUser function body
            span_start: 1537,
            span_end: 1541,
            infer_kind: 'function_return',
            alias: 'GetUserReturn',
          },
        ],
      });

      if (response.inferred_types && response.inferred_types.length > 0) {
        assert.strictEqual(response.inferred_types[0].alias, 'GetUserReturn');
      }
    });

    it('should infer variable types', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-4',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 13, // Inside db const declaration (findUser line)
            span_start: 288,
            span_end: 290,
            infer_kind: 'variable',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-4');
      if (response.inferred_types && response.inferred_types.length > 0) {
        assert.strictEqual(response.inferred_types[0].infer_kind, 'variable');
      }
    });

    it('should infer call result from the innermost call expression', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-5',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/call-site.ts'),
            line_number: 21,
            span_start: 376,
            span_end: 387,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-5');
      assert.ok(
        response.status === 'success' || response.inferred_types,
        'Should succeed or have inferred types'
      );
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'call_result');
        assert.ok(inferred.type_string.includes('User'));
        assert.ok(!inferred.type_string.includes('RegisterHandle'));
      }
    });

    it('should prefer response.json payload types for fetch calls', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-6',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/fetch-json.ts'),
            line_number: 2,
            span_start: 60,
            span_end: 96,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-6');
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'call_result');
        assert.ok(inferred.type_string.includes('id'));
        assert.ok(inferred.type_string.includes('name'));
        assert.ok(!inferred.type_string.includes('Response'));
      }
    });

    // TODO: Re-enable after full def-use tracking reimplementation
    // This test requires complex def-use analysis that was simplified in the refactor
    it.skip('should follow call results through destructuring', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-7',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/def-use.ts'),
            line_number: 18,
            span_start: 311,
            span_end: 327,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-7');
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'call_result');
        assert.ok(inferred.type_string.includes('UserData'));
        assert.ok(!inferred.type_string.includes('UserResponse'));
        assert.ok(!inferred.type_string.includes('Promise'));
      }
    });

    it('should infer call results for chained expressions', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-8',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/def-use.ts'),
            line_number: 23,
            span_start: 408,
            span_end: 444,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-8');
      if (response.inferred_types && response.inferred_types.length > 0) {
        const inferred = response.inferred_types[0];
        assert.strictEqual(inferred.infer_kind, 'call_result');
        assert.ok(inferred.type_string.includes('UserData'));
        assert.ok(!inferred.type_string.includes('Promise'));
      }
    });

    it('extraction config unwraps a node_modules wrapper via exact symbol match', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-9',
        extraction_config: {
          rules: [
            {
              wrapperSymbols: ['ApiResponse'],
              originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
              payloadGenericIndex: 0,
            },
          ],
        },
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/wrapper-usage.ts'),
            line_number: 20,
            span_start: 371,
            span_end: 389,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-9');
      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.strictEqual(inferred.infer_kind, 'call_result');
      assert.ok(
        inferred.type_string.includes('UserData'),
        `expected payload type, got: ${inferred.type_string}`
      );
      assert.ok(!inferred.type_string.includes('ApiResponse'));
    });

    it('origin-gated rule tolerates a glob list where only some entries match', async () => {
      // Ambient/global machinery types ship several candidate origin globs;
      // entries that match nothing must be no-ops, not failures.
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-10',
        extraction_config: {
          rules: [
            {
              machineryIndicators: ['data'],
              originModuleGlobs: [
                'definitely-not-installed/*',
                '@types/never-present/*',
                'wrapper-lib/*',
              ],
              payloadGenericIndex: 0,
            },
          ],
        },
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/wrapper-usage.ts'),
            line_number: 20,
            span_start: 371,
            span_end: 389,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-10');
      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.ok(
        inferred.type_string.includes('UserData'),
        `unmatched globs must be no-ops, got: ${inferred.type_string}`
      );
    });

    it('a matched rule that extracts nothing does not block later rules', async () => {
      // The model may emit several overlapping rules for one wrapper (e.g. a
      // generic-index variant and a property-path variant). A rule that
      // matches the wrapper but recovers no payload must let later rules try.
      const response = await client.send<{
        request_id: string;
        inferred_types?: Array<{ type_string: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-13',
        extraction_config: {
          rules: [
            {
              wrapperSymbols: ['ApiResponse'],
              originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
              payloadGenericIndex: 5, // out of range — extracts nothing
            },
            {
              wrapperSymbols: ['ApiResponse'],
              originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
              payloadGenericIndex: 5, // also out of range
              payloadPropertyPath: ['data'],
            },
          ],
        },
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/wrapper-usage.ts'),
            line_number: 20,
            span_start: 371,
            span_end: 389,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.ok(
        inferred.type_string.includes('UserData'),
        `later property-path rule must still extract, got: ${inferred.type_string}`
      );
    });

    it('origin-verified match with no recoverable payload collapses to unknown', async () => {
      // The wrapper's identity is proven (declaration in wrapper-lib), but no
      // rule can recover the payload: the machinery type itself is never the
      // contract, so the result degrades to `unknown` (treated downstream as
      // unresolved) instead of publishing ApiResponse<...>.
      const response = await client.send<{
        request_id: string;
        inferred_types?: Array<{ type_string: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-14',
        extraction_config: {
          rules: [
            {
              wrapperSymbols: ['ApiResponse'],
              originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
              payloadGenericIndex: 5, // out of range — extracts nothing
            },
          ],
        },
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/wrapper-usage.ts'),
            line_number: 20,
            span_start: 371,
            span_end: 389,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.strictEqual(
        inferred.type_string,
        'unknown',
        `verified machinery with no payload must collapse to unknown, got: ${inferred.type_string}`
      );
    });

    it('an ungated name-only match that extracts nothing leaves the type intact', async () => {
      // Without origin globs a wrapperSymbols hit is just a name match — not
      // proof of machinery. When nothing is extracted, the local type keeps
      // its real structural type instead of being stomped to unknown.
      const response = await client.send<{
        request_id: string;
        inferred_types?: Array<{ type_string: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-15',
        extraction_config: {
          rules: [
            {
              wrapperSymbols: ['ApiResponse'],
              payloadGenericIndex: 5, // out of range — extracts nothing
            },
          ],
        },
        requests: [
          {
            file_path: path.join(
              FIXTURES_PATH,
              'src/wrapper-false-positive.ts'
            ),
            line_number: 8,
            span_start: 197,
            span_end: 215,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.ok(
        inferred.type_string.includes('ApiResponse'),
        `ungated no-payload match must not stomp the type, got: ${inferred.type_string}`
      );
      assert.notStrictEqual(inferred.type_string, 'unknown');
    });

    it('origin globs are segment-bounded: a package-name prefix does not match', async () => {
      // `wrapper-li` must not match `node_modules/wrapper-lib` — otherwise a
      // glob like `got` would unwrap types from `got-scraping`.
      const response = await client.send<{
        request_id: string;
        inferred_types?: Array<{ type_string: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-16',
        extraction_config: {
          rules: [
            {
              machineryIndicators: ['data'],
              originModuleGlobs: ['wrapper-li'],
              payloadGenericIndex: 0,
            },
          ],
        },
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/wrapper-usage.ts'),
            line_number: 20,
            span_start: 371,
            span_end: 389,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.ok(
        inferred.type_string.includes('ApiResponse'),
        `prefix glob must not match, got: ${inferred.type_string}`
      );
    });

    it('function_return unwraps the wrapper inside an async Promise return', async () => {
      // An async handler returns Promise<ApiResponse<UserData>>; the rules
      // must apply to the awaited type (the Promise symbol matches no rule),
      // reproducing the deleted wrapper-registry's Promise<Wrapper<T>> → T.
      const response = await client.send<{
        request_id: string;
        inferred_types?: Array<{ type_string: string; infer_kind: string }>;
      }>({
        action: 'infer',
        request_id: 'infer-17',
        extraction_config: {
          rules: [
            {
              wrapperSymbols: ['ApiResponse'],
              originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
              payloadGenericIndex: 0,
            },
          ],
        },
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/wrapper-usage.ts'),
            line_number: 19,
            infer_kind: 'function_return',
          },
        ],
      });

      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.strictEqual(inferred.infer_kind, 'function_return');
      assert.ok(
        inferred.type_string.includes('UserData'),
        `expected the awaited payload, got: ${inferred.type_string}`
      );
      assert.ok(
        !inferred.type_string.includes('ApiResponse'),
        `wrapper must be unwrapped inside the Promise, got: ${inferred.type_string}`
      );
    });

    it('wrapperSymbols with origin globs do not match same-named local types', async () => {
      // `Response`-style names are shared by the DOM, frameworks, and HTTP
      // clients. When a rule carries origin globs, an exact symbol match must
      // still verify the declaration's origin.
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-12',
        extraction_config: {
          rules: [
            {
              wrapperSymbols: ['ApiResponse'],
              originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
              payloadGenericIndex: 0,
            },
          ],
        },
        requests: [
          {
            file_path: path.join(
              FIXTURES_PATH,
              'src/wrapper-false-positive.ts'
            ),
            line_number: 8,
            span_start: 197,
            span_end: 215,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-12');
      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.ok(
        inferred.type_string.includes('ApiResponse'),
        `symbol match must be origin-gated, got: ${inferred.type_string}`
      );
    });

    it('origin-gated rule leaves workspace-local lookalike types wrapped', async () => {
      // A local type named like a machinery wrapper, with matching indicator
      // properties, must NOT unwrap when its declaration does not come from
      // one of the origin globs.
      const response = await client.send<{
        request_id: string;
        status: string;
        inferred_types?: Array<{
          alias: string;
          type_string: string;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'infer-11',
        extraction_config: {
          rules: [
            {
              machineryIndicators: ['data'],
              originModuleGlobs: ['wrapper-lib', 'wrapper-lib/*'],
              payloadGenericIndex: 0,
            },
          ],
        },
        requests: [
          {
            file_path: path.join(
              FIXTURES_PATH,
              'src/wrapper-false-positive.ts'
            ),
            line_number: 8,
            span_start: 197,
            span_end: 215,
            infer_kind: 'call_result',
          },
        ],
      });

      assert.strictEqual(response.request_id, 'infer-11');
      assert.ok(response.inferred_types && response.inferred_types.length > 0);
      const inferred = response.inferred_types[0];
      assert.strictEqual(inferred.infer_kind, 'call_result');
      assert.ok(
        inferred.type_string.includes('ApiResponse'),
        `local lookalike must stay wrapped, got: ${inferred.type_string}`
      );
      assert.ok(inferred.type_string !== 'unknown');
    });

    it('signature_return keeps Promise wrapper and reports explicit', async () => {
      const response = await client.send<{
        inferred_types?: Array<{
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'sig-1',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 61,
            expression_text: 'res.json(user)',
            expression_line: 61,
            infer_kind: 'signature_return',
          },
        ],
      });

      const inferred = response.inferred_types?.[0];
      assert.ok(inferred, 'should infer getUser return');
      assert.strictEqual(inferred!.infer_kind, 'signature_return');
      // getUser is annotated `: Promise<void>` — must NOT be unwrapped to void.
      assert.strictEqual(inferred!.type_string, 'Promise<void>');
      assert.strictEqual(inferred!.is_explicit, true);
    });

    it('signature_return infers an unannotated return as implicit', async () => {
      const response = await client.send<{
        inferred_types?: Array<{
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'sig-2',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 87,
            expression_text: "console.log('Fetching orders for user:', userId)",
            expression_line: 87,
            infer_kind: 'signature_return',
          },
        ],
      });

      const inferred = response.inferred_types?.[0];
      assert.ok(inferred, 'should infer getOrders return');
      assert.strictEqual(inferred!.is_explicit, false);
      // Inferred return is a Promise (async handler) — wrapper preserved.
      assert.ok(inferred!.type_string.includes('Promise'));
    });

    it('function_param reads an annotated parameter as explicit', async () => {
      const response = await client.send<{
        inferred_types?: Array<{
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'param-1',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 61,
            expression_text: 'res.json(user)',
            expression_line: 61,
            infer_kind: 'function_param',
            param_name: 'req',
          },
        ],
      });

      const inferred = response.inferred_types?.[0];
      assert.ok(inferred, 'should infer getUser req param');
      assert.strictEqual(inferred!.infer_kind, 'function_param');
      assert.strictEqual(inferred!.type_string, 'Request');
      assert.strictEqual(inferred!.is_explicit, true);
    });

    it('function_param infers an unannotated parameter as implicit', async () => {
      const response = await client.send<{
        inferred_types?: Array<{
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'param-2',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 170,
            expression_text: 'n * 2',
            expression_line: 170,
            infer_kind: 'function_param',
            param_name: 'n',
          },
        ],
      });

      const inferred = response.inferred_types?.[0];
      assert.ok(inferred, 'should infer doubleIt n param');
      assert.strictEqual(inferred!.is_explicit, false);
      assert.ok(inferred!.type_string.length > 0);
    });

    it('resolves a function by line number alone (no span or text)', async () => {
      const response = await client.send<{
        inferred_types?: Array<{
          type_string: string;
          is_explicit: boolean;
          infer_kind: string;
        }>;
      }>({
        action: 'infer',
        request_id: 'line-1',
        requests: [
          {
            file_path: path.join(FIXTURES_PATH, 'src/routes.ts'),
            line_number: 169, // doubleIt declaration line — only locator provided
            infer_kind: 'signature_return',
          },
        ],
      });

      const inferred = response.inferred_types?.[0];
      assert.ok(inferred, 'should locate doubleIt by line alone');
      assert.strictEqual(inferred!.infer_kind, 'signature_return');
      assert.strictEqual(inferred!.is_explicit, false);
      assert.ok(inferred!.type_string.length > 0);
    });
  });

  describe('resolve_definitions action', () => {
    // First, bundle types to get the .d.ts content, then resolve from it
    let bundledDts: string;

    before(async () => {
      // Ensure initialized
      await client.send({
        action: 'init',
        request_id: 'resolve-setup',
        repo_root: FIXTURES_PATH,
      });

      // Bundle types to get .d.ts content we can resolve against
      const bundleResponse = await client.send<{
        status: string;
        dts_content?: string;
      }>({
        action: 'bundle',
        request_id: 'resolve-bundle',
        symbols: [
          { symbol_name: 'User', source_file: 'src/types.ts' },
          { symbol_name: 'UserProfile', source_file: 'src/types.ts' },
          { symbol_name: 'UserSettings', source_file: 'src/types.ts' },
          { symbol_name: 'UserRole', source_file: 'src/types.ts' },
          { symbol_name: 'Order', source_file: 'src/models.ts' },
          { symbol_name: 'OrderStatus', source_file: 'src/models.ts' },
        ],
      });

      assert.strictEqual(bundleResponse.status, 'success');
      assert.ok(bundleResponse.dts_content, 'Should have bundled .d.ts content');
      bundledDts = bundleResponse.dts_content;
    });

    it('should resolve a simple interface', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        definitions?: Array<{
          type_alias: string;
          definition: string;
          expanded: string;
        }>;
        errors?: string[];
      }>({
        action: 'resolve_definitions',
        request_id: 'resolve-1',
        stub_dir: stubDirFor(bundledDts),
        aliases: ['User'],
      });

      assert.strictEqual(response.request_id, 'resolve-1');
      assert.strictEqual(response.status, 'success');
      assert.ok(response.definitions);
      assert.strictEqual(response.definitions.length, 1);

      const def = response.definitions[0];
      assert.strictEqual(def.type_alias, 'User');
      // definition should contain the original interface text
      assert.ok(def.definition.includes('interface User'), 'definition should have original interface');
      assert.ok(def.definition.includes('name'), 'definition should include name field');
      // expanded is the structural form: members inlined, not the nominal name (#246)
      assert.ok(def.expanded, 'expanded should be present');
      assert.ok(def.expanded.startsWith('{'), 'expanded should be a structural object literal');
      assert.ok(def.expanded.includes('id: string'), 'expanded should inline id member');
      assert.ok(def.expanded.includes('name: string'), 'expanded should inline name member');
      // Date is a library type — it stays by name, not inlined to its members.
      assert.ok(def.expanded.includes('createdAt: Date'), 'expanded should keep Date by name');
    });

    it('should resolve a type alias with union', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        definitions?: Array<{
          type_alias: string;
          definition: string;
          expanded: string;
        }>;
      }>({
        action: 'resolve_definitions',
        request_id: 'resolve-2',
        stub_dir: stubDirFor(bundledDts),
        aliases: ['UserRole'],
      });

      assert.strictEqual(response.status, 'success');
      assert.ok(response.definitions);
      const def = response.definitions[0];
      assert.strictEqual(def.type_alias, 'UserRole');
      // The expanded form should preserve the union literals
      assert.ok(def.expanded.includes('admin'), 'expanded should preserve union member "admin"');
      assert.ok(def.expanded.includes('user'), 'expanded should preserve union member "user"');
      assert.ok(def.expanded.includes('guest'), 'expanded should preserve union member "guest"');
    });

    it('should resolve an interface with transitive dependencies', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        definitions?: Array<{
          type_alias: string;
          definition: string;
          expanded: string;
        }>;
      }>({
        action: 'resolve_definitions',
        request_id: 'resolve-3',
        stub_dir: stubDirFor(bundledDts),
        aliases: ['UserProfile'],
      });

      assert.strictEqual(response.status, 'success');
      assert.ok(response.definitions);
      const def = response.definitions[0];
      assert.strictEqual(def.type_alias, 'UserProfile');
      // The bundler now inlines nested named members at bundle time (the bundle
      // carries no `UserSettings` declaration, so a bare ref would dangle), so
      // the emitted declaration — and hence this re-parsed `definition` — is the
      // flattened structural form: no `UserSettings` name, no `extends` clause.
      assert.ok(
        !def.definition.includes('UserSettings'),
        'definition should inline UserSettings, not reference it by name',
      );
      assert.ok(
        /settings: \{[^}]*theme:/.test(def.definition),
        'definition should inline the UserSettings structure',
      );
      // expanded inlines the transitive members: the named `settings: UserSettings`
      // becomes its structural shape, and the inherited `User` members are flattened in (#246).
      assert.ok(def.expanded.startsWith('{'), 'expanded should be a structural object literal');
      assert.ok(
        !/settings: UserSettings/.test(def.expanded),
        'expanded must NOT leave the named member ref `settings: UserSettings`',
      );
      assert.ok(
        /settings: \{[^}]*theme:/.test(def.expanded),
        'expanded should inline UserSettings into the settings member structure',
      );
      assert.ok(def.expanded.includes('notifications: boolean'), 'expanded should inline a nested member');
      // Inherited (extends User) members are present structurally.
      assert.ok(def.expanded.includes('id: string'), 'expanded should flatten inherited User members');
    });

    it('should resolve multiple aliases in one call', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        definitions?: Array<{
          type_alias: string;
          definition: string;
          expanded: string;
        }>;
      }>({
        action: 'resolve_definitions',
        request_id: 'resolve-4',
        stub_dir: stubDirFor(bundledDts),
        aliases: ['User', 'Order', 'OrderStatus'],
      });

      assert.strictEqual(response.status, 'success');
      assert.ok(response.definitions);
      assert.strictEqual(response.definitions.length, 3);

      const names = response.definitions.map((d) => d.type_alias);
      assert.ok(names.includes('User'));
      assert.ok(names.includes('Order'));
      assert.ok(names.includes('OrderStatus'));

      // OrderStatus should have the union preserved
      const orderStatus = response.definitions.find((d) => d.type_alias === 'OrderStatus')!;
      assert.ok(orderStatus.expanded.includes('pending'));
      assert.ok(orderStatus.expanded.includes('shipped'));
    });

    it('should skip non-existent aliases without failing', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        definitions?: Array<{
          type_alias: string;
          definition: string;
          expanded: string;
        }>;
      }>({
        action: 'resolve_definitions',
        request_id: 'resolve-5',
        stub_dir: stubDirFor(bundledDts),
        aliases: ['User', 'NonExistentType'],
      });

      assert.strictEqual(response.status, 'success');
      assert.ok(response.definitions);
      // Should only resolve the one that exists
      assert.strictEqual(response.definitions.length, 1);
      assert.strictEqual(response.definitions[0].type_alias, 'User');
    });

    // #246: expanded_definition must be the fully-inlined STRUCTURAL form —
    // named member types expanded to their structure, recursively. These shapes
    // mirror the xrepo-corpus-1 gateway resolver (Money / ApiResponse<T> /
    // OrderStatus / optional note) and pin the exact strings the eval scores.
    describe('expanded inlines named member types (#246)', () => {
      // Self-contained .d.ts equivalent to the corpus gateway shapes.
      const corpusDts = [
        'export interface Money { amountCents: number; currency: string }',
        'export type OrderStatus =',
        '  | { kind: "placed"; placedAt: string }',
        '  | { kind: "refunded"; refundedAt: string; reason?: string };',
        'export interface Order { id: string; total: Money; status: OrderStatus; note?: string }',
        'export interface ApiResponse<T> { data: T; errors: string[] }',
        'export type Endpoint_OrderResp = ApiResponse<Order>;',
        'export type Endpoint_OrdersResp = Order[];',
        'export type Endpoint_OrderUpdated = Order;',
        'export type WithDate = { id: string; created: Date };',
        'export type Pair = [number, string];',
      ].join('\n');

      async function resolveOne(
        requestId: string,
        alias: string,
      ): Promise<string> {
        const response = await client.send<{
          status: string;
          definitions?: Array<{
            type_alias: string;
            definition: string;
            expanded: string;
          }>;
        }>({
          action: 'resolve_definitions',
          request_id: requestId,
          stub_dir: stubDirFor(corpusDts),
          aliases: [alias],
        });
        assert.strictEqual(response.status, 'success');
        const def = response.definitions?.find((d) => d.type_alias === alias);
        assert.ok(def, `expected a definition for ${alias}`);
        return def.expanded;
      }

      it('inlines a nested named object (Money -> { amountCents; currency })', async () => {
        const expanded = await resolveOne('resolve-money', 'Endpoint_OrderUpdated');
        assert.ok(
          !/total: Money/.test(expanded),
          'must NOT leave the named member ref `total: Money`',
        );
        assert.ok(
          expanded.includes('total: { amountCents: number; currency: string; }'),
          `nested Money should inline structurally; got: ${expanded}`,
        );
      });

      it('inlines a generic wrapper (ApiResponse<Order>) fully', async () => {
        const expanded = await resolveOne('resolve-generic', 'Endpoint_OrderResp');
        assert.strictEqual(
          expanded,
          '{ data: { id: string; total: { amountCents: number; currency: string; }; status: { kind: "placed"; placedAt: string; } | { kind: "refunded"; refundedAt: string; reason?: string; }; note?: string; }; errors: string[]; }',
        );
      });

      it('inlines a discriminated union field (OrderStatus)', async () => {
        const expanded = await resolveOne('resolve-union', 'OrderStatus');
        assert.strictEqual(
          expanded,
          '{ kind: "placed"; placedAt: string; } | { kind: "refunded"; refundedAt: string; reason?: string; }',
        );
      });

      it('renders an optional field as `note?: string` (strips undefined)', async () => {
        const expanded = await resolveOne('resolve-optional', 'Endpoint_OrderUpdated');
        assert.ok(
          expanded.includes('note?: string'),
          `optional note should render as note?: string; got: ${expanded}`,
        );
        assert.ok(
          !expanded.includes('note?: string | undefined'),
          'optional field must not carry an explicit `| undefined`',
        );
      });

      it('inlines an array element while keeping the [] suffix', async () => {
        const expanded = await resolveOne('resolve-array', 'Endpoint_OrdersResp');
        assert.ok(expanded.endsWith('}[]'), `should be a structural object array; got: ${expanded}`);
        assert.ok(expanded.includes('total: { amountCents: number; currency: string; }'));
      });

      it('keeps library types by name (Date is not inlined)', async () => {
        const expanded = await resolveOne('resolve-date', 'WithDate');
        assert.strictEqual(expanded, '{ id: string; created: Date; }');
      });

      it('keeps tuple types as `[a, b]` without exploding into Array.prototype', async () => {
        const expanded = await resolveOne('resolve-tuple', 'Pair');
        assert.strictEqual(expanded, '[number, string]');
      });
    });
  });

  describe('error handling', () => {
    it('should handle invalid JSON gracefully', async () => {
      // This is tricky to test since we can't send invalid JSON through our client
      // Skip this test as it requires lower-level access
    });

    it('should reject unknown actions', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        errors?: string[];
      }>({
        action: 'unknown_action',
        request_id: 'error-1',
      } as Record<string, unknown>);

      assert.strictEqual(response.status, 'error');
      assert.ok(response.errors && response.errors.length > 0);
    });

    it('should reject requests without required fields', async () => {
      const response = await client.send<{
        request_id: string;
        status: string;
        errors?: string[];
      }>({
        action: 'init',
        // Missing request_id and repo_root
      } as Record<string, unknown>);

      assert.strictEqual(response.status, 'error');
    });

    it('should handle bundle before init', async () => {
      // Start a fresh client to test uninitialized state
      const freshClient = new SidecarClient();
      await freshClient.start();

      try {
        const response = await freshClient.send<{
          request_id: string;
          status: string;
          errors?: string[];
        }>({
          action: 'bundle',
          request_id: 'uninit-1',
          symbols: [{ symbol_name: 'User', source_file: 'src/types.ts' }],
        });

        assert.strictEqual(response.status, 'error');
        assert.ok(response.errors?.some((e) => e.toLowerCase().includes('init')));
      } finally {
        await freshClient.stop();
      }
    });
  });
});

// ===========================================================================
// Unit-style tests for validators
// ===========================================================================

describe('Validator Unit Tests', () => {
  // Import dynamically to avoid issues if build hasn't run
  it('should validate init request', async () => {
    const { parseRequest } = await import('../src/validators.js');

    const result = parseRequest({
      action: 'init',
      request_id: 'test-1',
      repo_root: '/some/path',
    });

    assert.strictEqual(result.success, true);
    if (result.success) {
      assert.strictEqual(result.request.action, 'init');
    }
  });

  it('should reject invalid init request', async () => {
    const { parseRequest } = await import('../src/validators.js');

    const result = parseRequest({
      action: 'init',
      request_id: 'test-1',
      // Missing repo_root
    });

    assert.strictEqual(result.success, false);
  });

  it('should validate bundle request', async () => {
    const { parseRequest } = await import('../src/validators.js');

    const result = parseRequest({
      action: 'bundle',
      request_id: 'test-2',
      symbols: [
        {
          symbol_name: 'User',
          source_file: 'src/types.ts',
        },
      ],
    });

    assert.strictEqual(result.success, true);
  });

  it('should validate infer request', async () => {
    const { parseRequest } = await import('../src/validators.js');

    const result = parseRequest({
      action: 'infer',
      request_id: 'test-3',
      requests: [
        {
          file_path: 'src/routes.ts',
          line_number: 10,
          span_start: 10,
          span_end: 20,
          infer_kind: 'function_return',
        },
      ],
    });

    assert.strictEqual(result.success, true);
  });

  it('should validate resolve_definitions request', async () => {
    const { parseRequest } = await import('../src/validators.js');

    const result = parseRequest({
      action: 'resolve_definitions',
      request_id: 'test-resolve-1',
      stub_dir: stubDirFor('export type Foo = string;'),
      aliases: ['Foo'],
    });

    assert.strictEqual(result.success, true);
  });

  it('should reject resolve_definitions with empty aliases', async () => {
    const { parseRequest } = await import('../src/validators.js');

    const result = parseRequest({
      action: 'resolve_definitions',
      request_id: 'test-resolve-2',
      stub_dir: stubDirFor('export type Foo = string;'),
      aliases: [],
    });

    assert.strictEqual(result.success, false);
  });

  it('should reject resolve_definitions with empty stub_dir', async () => {
    const { parseRequest } = await import('../src/validators.js');

    const result = parseRequest({
      action: 'resolve_definitions',
      request_id: 'test-resolve-3',
      stub_dir: '',
      aliases: ['Foo'],
    });

    assert.strictEqual(result.success, false);
  });

  it('should reject invalid infer_kind', async () => {
    const { parseRequest } = await import('../src/validators.js');

    const result = parseRequest({
      action: 'infer',
      request_id: 'test-4',
      requests: [
        {
          file_path: 'src/routes.ts',
          line_number: 10,
          span_start: 10,
          span_end: 20,
          infer_kind: 'invalid_kind',
        },
      ],
    });

    assert.strictEqual(result.success, false);
  });
});
