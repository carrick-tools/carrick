/**
 * Readiness must not scale with the scanned repo's installed dependencies
 * (carrick#749, sidecar half of carrick#748).
 *
 * The 0.3.42 Action installs the scanned repo's dependencies, and on a large
 * monorepo that turned a 0.5 s init into a 40 s one, past the readiness budget,
 * dropping the whole type layer. None of that time was compilation: the
 * default-pattern fallback used ts-morph's `addSourceFilesAtPaths`, which
 * recursively registers every descendant directory of any directory a glob hit
 * in — the repo root, and so `node_modules` with it.
 *
 * Three properties are asserted:
 *  - reaching a usable project costs the same over a large installed tree as
 *    over a small one;
 *  - `node_modules` is never registered and never contributes a source file;
 *  - `init` answers without building the program at all, and a later request
 *    still gets a working one.
 *
 * The scaling assertion compares a small tree against a large one rather than
 * checking an absolute wall-clock bound, so a slow or loaded machine moves both
 * arms together and the test still measures scaling.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { ProjectLoader } from '../src/project-loader.js';
import { SidecarClient } from './helpers.js';

/** Packages in the "installed" arm. 8,000 packages is 24,000 directories. */
const LARGE_PACKAGE_COUNT = 8000;
/** Packages in the control arm: an installed tree small enough to be free. */
const SMALL_PACKAGE_COUNT = 20;

/**
 * A repo whose only TypeScript file sits at the root — the shape that triggers
 * the walk, because a root-level glob hit makes the repo root a globbed
 * directory — with `count` packages installed under `node_modules`.
 *
 * Declarations go in every fiftieth package: the walk costs per directory, so
 * the directories are what has to be real, and writing 8,000 files would cost
 * more to set up than the behaviour under test costs to trigger.
 */
function makeRepo(count: number): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-init-scale-'));
  fs.writeFileSync(
    path.join(root, 'index.ts'),
    'export interface RootShape { id: string; }\nexport const root: RootShape = { id: "1" };\n'
  );
  for (let i = 0; i < count; i++) {
    const pkg = path.join(root, 'node_modules', `pkg-${i}`, 'dist', 'esm');
    fs.mkdirSync(pkg, { recursive: true });
    if (i % 50 === 0) {
      fs.writeFileSync(
        path.join(pkg, 'index.d.ts'),
        'export declare const value: number;\n'
      );
    }
  }
  return root;
}

/**
 * Time everything between "here is a repo root" and "here is a project I can
 * ask questions of". Measuring the total, not one phase of it, is what makes
 * the assertion hold whatever the split between load and build is.
 */
function msToUsableProject(repoRoot: string): { totalMs: number; loader: ProjectLoader } {
  const start = performance.now();
  const loader = new ProjectLoader({ repoRoot });
  const result = loader.load();
  assert.ok(result.success, `load failed: ${result.error}`);
  loader.getProject();
  return { totalMs: performance.now() - start, loader };
}

describe('init does not scale with an installed node_modules', () => {
  let smallRepo: string;
  let largeRepo: string;

  before(() => {
    smallRepo = makeRepo(SMALL_PACKAGE_COUNT);
    largeRepo = makeRepo(LARGE_PACKAGE_COUNT);
  });

  after(() => {
    for (const dir of [smallRepo, largeRepo]) {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  // Runs first on purpose: it is the timed one, and a build of the large tree
  // in an earlier test would leave the directory cache warm and flatter the
  // behaviour it is here to catch.
  it('costs the same to reach a usable project over 20 installed packages as over 8,000', () => {
    const small = msToUsableProject(smallRepo);
    const large = msToUsableProject(largeRepo);

    // A difference of this size cannot be the 24,000-directory walk: on the
    // machine this was written on the walk alone cost 887 ms, and it grows
    // with the installed tree, which is the property under test.
    assert.ok(
      large.totalMs - small.totalMs < 250,
      `reaching a usable project scaled with node_modules: ` +
        `${Math.round(small.totalMs)}ms for ${SMALL_PACKAGE_COUNT} packages vs ` +
        `${Math.round(large.totalMs)}ms for ${LARGE_PACKAGE_COUNT}`
    );
  });

  it('never registers node_modules or reads a file out of it', () => {
    const { loader } = msToUsableProject(largeRepo);
    const project = loader.getProject();

    assert.strictEqual(
      project.getDirectory(path.join(largeRepo, 'node_modules')),
      undefined,
      'node_modules must not be registered as a project directory'
    );

    const fromNodeModules = project
      .getSourceFiles()
      .map((f) => f.getFilePath() as string)
      .filter((p) => p.includes('/node_modules/'));
    assert.deepStrictEqual(
      fromNodeModules,
      [],
      'no source file may come from node_modules'
    );

    assert.ok(
      project.getSourceFiles().some((f) => f.getBaseName() === 'index.ts'),
      'the repo root source file must still be added'
    );
  });

  it('answers init without building the program, then builds on demand', async () => {
    const client = new SidecarClient();
    await client.start();
    try {
      // One round trip first, so what follows times the init handler rather
      // than the Node process still loading the compiler.
      await client.send({ action: 'health', request_id: 'init-scale-0' });

      const start = performance.now();
      const response = await client.send<{ status: string; init_time_ms?: number }>({
        action: 'init',
        request_id: 'init-scale-1',
        repo_root: largeRepo,
      });
      const elapsed = performance.now() - start;

      assert.strictEqual(response.status, 'ready');
      assert.ok(
        elapsed < 500,
        `init took ${Math.round(elapsed)}ms over an installed tree; readiness ` +
          `must not wait on the program`
      );

      // The program is still there when a request needs it.
      const inferred = await client.send<{
        status: string;
        inferred_types?: Array<{ type_string?: string }>;
      }>({
        action: 'infer',
        request_id: 'init-scale-2',
        requests: [
          {
            file_path: path.join(largeRepo, 'index.ts'),
            line_number: 2,
            expression_text: 'root',
            expression_line: 2,
            infer_kind: 'variable',
          },
        ],
      });
      assert.strictEqual(
        inferred.status,
        'success',
        `deferred build did not serve the request: ${JSON.stringify(inferred)}`
      );
      assert.strictEqual(
        inferred.inferred_types?.[0]?.type_string,
        'RootShape',
        `expected the root file's type, got ${JSON.stringify(inferred.inferred_types)}`
      );
    } finally {
      await client.stop();
    }
  });
});
