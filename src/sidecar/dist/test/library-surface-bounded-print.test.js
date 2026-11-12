/**
 * carrick#3744: a type print that would expand without bound is listed as
 * `<not printed: too large>` instead of running the lister out of heap. The
 * sidecar runs at the library builder's child heap (1630 MB). Both packages
 * are invented (see deep-print-fixture.ts).
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { writeDeepPrintProject } from './deep-print-fixture.js';
const NOT_PRINTED = '<not printed: too large>';
describe('list_library_surface: bounded print (carrick#3744)', () => {
    let client;
    let root;
    let requestId = 0;
    const list = (packages) => client.send({ request_id: `p-${requestId++}`, action: 'list_library_surface', from_dir: root, packages, handlers: true, types: true, max_entries: 1_000_000 }, 120_000);
    const returnsOf = (listed, pkg, name) => listed.surfaces
        .find(s => s.package === pkg)
        .exports.find(e => e.export === name)
        .receivers.find(r => r.receiver === 'export').call[0].returns;
    before(async () => {
        root = fs.mkdtempSync(path.join(os.tmpdir(), 'bounded-print-'));
        writeDeepPrintProject(root);
        client = new SidecarClient();
        await client.start({ NODE_OPTIONS: '--max-old-space-size=1630' });
        const init = await client.send({ request_id: 'init', action: 'init', repo_root: root });
        assert.strictEqual(init.status, 'ready');
    });
    after(async () => {
        await client.stop();
        fs.rmSync(root, { recursive: true, force: true });
    });
    it('lists a print that doubles at every level as not printed, and the rest whole', async () => {
        const deep = await list(['fixture-deep-print']);
        assert.strictEqual(deep.status, 'success', JSON.stringify(deep.errors));
        assert.strictEqual(returnsOf(deep, 'fixture-deep-print', 'flat'), NOT_PRINTED);
        assert.strictEqual(returnsOf(deep, 'fixture-deep-print', 'plain'), 'Leaf<string>');
        const plain = await list(['fixture-plain-print']);
        const entry = (listed, pkg) => JSON.stringify(listed.surfaces.find(s => s.package === pkg).exports.find(e => e.export === 'plain'));
        assert.strictEqual(entry(deep, 'fixture-deep-print'), entry(plain, 'fixture-plain-print'));
        const again = await list(['fixture-deep-print']);
        assert.strictEqual(again.surface_sha256, deep.surface_sha256);
    });
});
