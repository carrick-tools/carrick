/**
 * Reading a Deno service writes nothing into the service's own folder. The
 * graph root Deno reads used to be a file at `<service>/.carrick/deno/graph.ts`:
 * it showed in `git status` while the engine ran, and the folder it left behind
 * made a checkout look like a workspace of its own (a nested git worktree, in
 * particular, stopped resolving the workspace's index).
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { ProjectLoader } from '../src/project-loader.js';
import { captureStub, DenoProject, findDenoConfig } from '../src/capture/index.js';

const hasDeno = spawnSync('deno', ['--version']).status === 0;

/** Every path under `dir`, relative, folders marked with a trailing slash. */
function listing(dir: string): string[] {
  const out: string[] = [];
  const walk = (current: string): void => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      out.push(path.relative(dir, full) + (entry.isDirectory() ? '/' : ''));
      if (entry.isDirectory()) walk(full);
    }
  };
  walk(dir);
  return out.sort();
}

/** A Deno workspace with one member whose own import map names a global-types file. */
function workspace(): { root: string; service: string } {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-writes-'));
  const service = path.join(root, 'app');
  fs.mkdirSync(service);
  fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({ workspace: ['./app'] }));
  fs.writeFileSync(path.join(service, 'deno.json'), JSON.stringify({
    imports: { '@globals': './globals.d.ts' },
    compilerOptions: { types: ['@globals'] },
  }));
  fs.writeFileSync(path.join(service, 'globals.d.ts'), 'interface DomainGlobal { status: "configured" }');
  fs.writeFileSync(path.join(service, 'main.ts'), 'export type Configured = DomainGlobal;\nexport const row = { id: "a", amount: 1 };\n');
  return { root, service };
}

describe('Deno project writes', { skip: !hasDeno }, () => {
  it('leaves the service folder exactly as it found it, and the workspace root holds the cache', () => {
    const { root, service } = workspace();
    try {
      const before = listing(root);
      const project = new DenoProject(findDenoConfig(service)!, service);
      assert.deepEqual(project.diagnostics, []);
      const after = listing(root);
      assert.deepEqual(after.filter(p => p.startsWith('app/')), before.filter(p => p.startsWith('app/')), 'the service folder is untouched');
      assert.equal(fs.existsSync(path.join(service, '.carrick')), false);
      assert.ok(after.some(p => /^\.carrick\/deno\/[0-9a-f]+\/$/.test(p)), after.join('\n'));
      assert.equal(after.some(p => p.endsWith('graph.ts')), false, 'the graph root is removed once Deno has read it');
      // The folder a re-check may create holds nothing git would track.
      assert.equal(fs.readFileSync(path.join(root, '.carrick', '.gitignore'), 'utf8').split('\n').includes('*'), true);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('still reads the member import map for a bare `types` entry, and a capture adds nothing under the service', () => {
    const { root, service } = workspace();
    const out = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-writes-out-'));
    try {
      const loader = new ProjectLoader({ repoRoot: service });
      assert.equal(loader.load().success, true);
      const missing = loader.getProject().getPreEmitDiagnostics().filter(d => [2304, 2307].includes(d.getCode()));
      assert.deepEqual(missing.map(d => d.getMessageText()), []);
      const captured = captureStub({ repoRoot: service, serviceName: 'app', outDir: path.join(out, 'stub'), anchors: [
        { kind: 'symbol', alias: 'Configured', symbol_name: 'Configured', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
      ] });
      assert.equal(captured.success, true, captured.errors.join('\n'));
      assert.ok(captured.aliases.every(a => a.self_check === 'ok'), JSON.stringify(captured.aliases));
      assert.equal(fs.existsSync(path.join(service, '.carrick')), false, listing(service).join('\n'));
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
      fs.rmSync(out, { recursive: true, force: true });
    }
  });

  it('an empty .carrick/deno/ left in the service by an older version changes nothing', () => {
    const { root, service } = workspace();
    try {
      fs.mkdirSync(path.join(service, '.carrick', 'deno'), { recursive: true });
      const project = new DenoProject(findDenoConfig(service)!, service);
      assert.deepEqual(project.diagnostics, []);
      assert.deepEqual(listing(path.join(service, '.carrick')), ['deno/']);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('leaves a .gitignore the workspace root already has in .carrick/ as it is', () => {
    const { root, service } = workspace();
    try {
      fs.mkdirSync(path.join(root, '.carrick'));
      const own = '# the user\'s own rules\nhosted/\n';
      fs.writeFileSync(path.join(root, '.carrick', '.gitignore'), own);
      const project = new DenoProject(findDenoConfig(service)!, service);
      assert.deepEqual(project.diagnostics, []);
      assert.equal(fs.readFileSync(path.join(root, '.carrick', '.gitignore'), 'utf8'), own);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
