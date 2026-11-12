/**
 * carrick#3246: a check killed at its budget must not leave its surface entry
 * in the user's tree, and the next capture clears one a SIGKILL left behind.
 */
import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as os from 'node:os';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { removeStaleSurfaceEntries, WriteGuard } from '../src/capture/index.js';
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CAPTURE_MODULE = path.join(__dirname, '..', 'src', 'capture', 'index.js');
/** A pid that is not running: a child that has exited and been reaped. */
async function deadPid() {
    const child = spawn(process.execPath, ['-e', '0']);
    await new Promise((resolve) => child.on('exit', resolve));
    return child.pid;
}
describe('surface entry cleanup (carrick#3246)', () => {
    it('removes an entry whose process is gone and keeps a running one', async () => {
        const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-stale-entry-'));
        const dead = path.join(dir, `__carrick_surface__.${await deadPid()}.1.ts`);
        const live = path.join(dir, `__carrick_surface__.${process.ppid}.1.ts`);
        const own = path.join(dir, `__carrick_surface__.${process.pid}.1.ts`);
        const source = path.join(dir, 'index.ts');
        for (const file of [dead, live, own, source])
            fs.writeFileSync(file, '');
        removeStaleSurfaceEntries(dir, WriteGuard.of({ dirs: [dir] }));
        assert.ok(!fs.existsSync(dead), 'a dead process\'s entry is removed');
        assert.ok(fs.existsSync(live), 'a running process\'s entry stays');
        assert.ok(fs.existsSync(own), 'this process\'s own entry stays');
        assert.ok(fs.existsSync(source), 'a source file stays');
        fs.rmSync(dir, { recursive: true, force: true });
    });
    it('removes a held entry when the process is terminated', async () => {
        const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-held-entry-'));
        const entry = path.join(dir, '__carrick_surface__.1.1.ts');
        // The child stands in for the sidecar: it holds an entry and, as the
        // sidecar's SIGTERM handler does, exits through process.exit.
        const script = `
      import { holdSurfaceEntry, WriteGuard } from ${JSON.stringify(CAPTURE_MODULE)};
      import * as fs from 'node:fs';
      fs.writeFileSync(${JSON.stringify(entry)}, '');
      holdSurfaceEntry(${JSON.stringify(entry)}, WriteGuard.of({ files: [${JSON.stringify(entry)}] }));
      process.on('SIGTERM', () => process.exit(0));
      console.log('ready');
      setInterval(() => {}, 1000);
    `;
        const child = spawn(process.execPath, ['--input-type=module', '-e', script], {
            stdio: ['ignore', 'pipe', 'inherit'],
        });
        await new Promise((resolve) => child.stdout.once('data', () => resolve()));
        assert.ok(fs.existsSync(entry), 'the entry exists while the capture runs');
        const exited = new Promise((resolve) => child.on('exit', resolve));
        child.kill('SIGTERM');
        await exited;
        assert.ok(!fs.existsSync(entry), 'the entry is gone after SIGTERM');
        fs.rmSync(dir, { recursive: true, force: true });
    });
});
