// Consume actual Rust check/status output through the npm decoder, hook
// processes and LSP server. The native integration test supplies the payload.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const { check, status, root } = JSON.parse(fs.readFileSync(0, 'utf8'));
const npm = process.env.CARRICK_CONSUMER_SOURCE;
const load = relative => import(pathToFileURL(path.join(npm, relative)).href);
const { parseCheckResult, parseStatusResult } = await load('src/contract.ts');
const { definitionsAt } = await load('src/definition.ts');
const { toDiagnostics } = await load('src/diagnostics.ts');
const { renderPostToolUse, renderSessionStart } = await load('src/render.ts');
const { fakeEnv, runHook, editPayload } = await load('test/helpers.ts');
const { LspClient } = await load('test/lsp-client.ts');
const decoded = parseCheckResult(JSON.stringify(check));
assert.ok(decoded);
assert.equal(decoded.items.length, check.items.length);
assert.deepEqual(decoded.boundary_lines, check.boundary_lines);
const file = path.join(check.repo, check.file);
for (const item of decoded.items) {
  assert.deepEqual(definitionsAt(decoded, { line: item.line - 1, character: 0 }, { exists: () => true }), []);
}
for (const target of toDiagnostics(decoded, root, file, { exists: () => true }).keys()) {
  assert.equal(target, file, 'a hosted counterpart became a local diagnostic target');
}
assert.ok(renderPostToolUse(decoded).includes(check.boundary_lines[0]));
const decodedStatus = parseStatusResult(JSON.stringify(status));
assert.ok(decodedStatus);
assert.equal(decodedStatus.services.length, status.services.length);
assert.ok(renderSessionStart(decodedStatus).includes(status.services[0].boundary_lines[0]));

const scratch = fs.mkdtempSync(path.join(root, '.carrick', 'consumer-'));
try {
  const checkFile = path.join(scratch, 'check.json');
  const statusFile = path.join(scratch, 'status.json');
  fs.writeFileSync(checkFile, JSON.stringify(check));
  fs.writeFileSync(statusFile, JSON.stringify(status));
  const env = fakeEnv({ CARRICK_FAKE_FIXTURE: checkFile, CARRICK_FAKE_REBASE: '0', CARRICK_CHANNEL: 'hooks', CARRICK_TOKEN: 'synthetic-unused' });
  const edited = await runHook('post-edit.ts', { payload: editPayload({ root, file }), env, cwd: root });
  assert.equal(edited.code, 0, edited.stderr);
  assert.ok(JSON.parse(edited.stdout).hookSpecificOutput.additionalContext.includes(check.boundary_lines[0]));
  const session = await runHook('session-start.ts', { env: { ...env, CARRICK_FAKE_FIXTURE: statusFile }, cwd: root });
  assert.equal(session.code, 0, session.stderr);
  assert.ok(session.stdout.includes(status.services[0].boundary_lines[0]));
  const client = new LspClient({ env: { ...env, CARRICK_CHANNEL: 'lsp' } });
  try {
    await client.initialize(root);
    client.open(file);
    await client.waitFor(() => client.publishes.length > 0, 'local diagnostics');
    assert.ok(client.publishes.every(p => p.uri === pathToFileURL(file).href));
    const request = client.definition(file, check.items[0].line - 1, 0);
    await client.waitFor(() => client.responses.has(request), 'definition response');
    assert.deepEqual(client.responses.get(request), []);
  } finally { client.stop(); }
} finally { fs.rmSync(scratch, { recursive: true, force: true }); }
