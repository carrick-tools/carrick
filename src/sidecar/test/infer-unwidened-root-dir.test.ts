/**
 * carrick#2898: the unwidened reading must not depend on which request built
 * the program last. A service imports a workspace package that sits outside
 * its `rootDir` (through a `node_modules` symlink), so the compiler reports
 * TS6059 at the import. That diagnostic belongs to the program, not to the
 * marking; it used to taint the reading on the first batch of a process and
 * not on the next, so a process that had answered before differed from a
 * fresh one.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const HANDLER = `import { fromContracts } from '@ws/contracts';
export function getThing() {
  return { ...fromContracts(), kind: 'x' };
}
`;

interface Inferred {
  alias: string;
  type_string: string;
  unwidened_type_string?: string;
}

const collapse = (text: string | undefined): string | undefined => text?.replace(/\s+/g, ' ').trim();

describe('carrick#2898: a rootDir diagnostic does not change the unwidened reading', () => {
  let root: string;
  let svc: string;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2898-'));
    const contracts = path.join(root, 'contracts');
    fs.mkdirSync(path.join(contracts, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(contracts, 'package.json'),
      JSON.stringify({ name: '@ws/contracts', version: '1.0.0', types: 'src/index.ts' })
    );
    fs.writeFileSync(
      path.join(contracts, 'src', 'index.ts'),
      'export function fromContracts(): { id: string } {\n  return { id: "a" };\n}\n'
    );
    svc = path.join(root, 'svc');
    fs.mkdirSync(path.join(svc, 'src'), { recursive: true });
    fs.mkdirSync(path.join(svc, 'node_modules', '@ws'), { recursive: true });
    fs.symlinkSync(contracts, path.join(svc, 'node_modules', '@ws', 'contracts'), 'dir');
    fs.writeFileSync(
      path.join(svc, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          target: 'es2020',
          module: 'commonjs',
          moduleResolution: 'node',
          strict: true,
          skipLibCheck: true,
          types: [],
          rootDir: '.',
        },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(svc, 'package.json'), JSON.stringify({ name: 'svc', private: true }));
    fs.writeFileSync(path.join(svc, 'src', 'handler.ts'), HANDLER);
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  const batch = () => ({
    action: 'infer',
    request_id: 'b',
    requests: [
      {
        file_path: path.join(svc, 'src', 'handler.ts'),
        line_number: 2,
        infer_kind: 'response_body',
        alias: 'thing',
      },
    ],
  });

  async function ask(client: SidecarClient, times: number): Promise<(string | undefined)[]> {
    const out: (string | undefined)[] = [];
    for (let i = 0; i < times; i++) {
      const res = await client.send<{ status: string; inferred_types?: Inferred[] }>(batch());
      assert.strictEqual(res.status, 'success', JSON.stringify(res));
      out.push(collapse(res.inferred_types?.[0]?.unwidened_type_string));
    }
    return out;
  }

  it('reads the same on the first batch, a repeated batch and a fresh process', async () => {
    const held = new SidecarClient();
    await held.start();
    await held.send({ action: 'init', request_id: 'init', repo_root: svc });
    const [first, second] = await ask(held, 2);
    await held.stop();

    const fresh = new SidecarClient();
    await fresh.start();
    await fresh.send({ action: 'init', request_id: 'init', repo_root: svc });
    const [cold] = await ask(fresh, 1);
    await fresh.stop();

    assert.ok(first, 'the first batch must carry a reading');
    assert.match(first, /kind: "x"/);
    assert.strictEqual(second, first, 'a repeated batch in the same process');
    assert.strictEqual(cold, first, 'a fresh process');
  });
});
