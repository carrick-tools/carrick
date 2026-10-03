/**
 * carrick#1776: a service whose tsconfig sits in an ancestor directory, with
 * no tsconfig named for it, was typed with the sidecar's default options.
 *
 * Both readers looked for a config in the service root only. A layout that
 * keeps one tsconfig above several client services (their module resolution,
 * `paths`, `lib`) got none of it: `paths` imports and the libraries the
 * config's resolution finds never resolved, and every type read off them was
 * `any`.
 *
 * A service with no config of its own now takes the nearest `tsconfig.json`
 * above it, found the way `tsc` finds one, walking up from the service root
 * and stopping at the scan root the caller names. Both readers take the same
 * answer:
 *  - the init'd project (`infer`, `bundle`, `retype_check`);
 *  - capture (`capture_v2`), the served surface.
 * Without a scan root, only the service root is searched.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import type { CaptureStubResult } from '../src/capture/api.js';
import { SidecarClient } from './helpers.js';

const CLIENT_TS = `import type { Profile } from "@shared/profile";

export async function profile(): Promise<Profile> {
  const res = await fetch("/api/v1/profile");
  return (await res.json()) as Profile;
}
`;
/** 1-based lines in CLIENT_TS. */
const PROFILE_FN_LINE = 3;
const PROFILE_BODY_LINE = 5;

/**
 * The ancestor's options. Each differs observably from the sidecar's
 * defaults: `paths` makes the import resolve at all, and `target` is
 * recorded in the stub's tsconfig snapshot.
 */
const ANCESTOR_CONFIG = {
  include: ['clients', 'shared'],
  compilerOptions: {
    target: 'ES2019',
    module: 'ESNext',
    moduleResolution: 'Bundler',
    strict: true,
    skipLibCheck: true,
    paths: { '@shared/*': ['./shared/*'] },
  },
};

const RESOLVED = '{ handle: string; bio: null | string; }';

const tempRoots: string[] = [];

after(() => {
  for (const root of tempRoots) fs.rmSync(root, { recursive: true, force: true });
});

interface Layout {
  /** The scanned repo's root. */
  root: string;
  /** The service directory, two levels below the config that types it. */
  service: string;
}

/**
 * A repo with one tsconfig in `apps/web/` over its client services in
 * `apps/web/clients/<name>/`, none of which has a config of its own.
 * `serviceConfig`, when given, is written as the service's own config, under
 * `serviceConfigName`.
 */
function writeRepo(serviceConfig?: unknown, serviceConfigName = 'tsconfig.json'): Layout {
  // realpath: on macOS the temp dir is a symlink, and the walk compares paths.
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1776-')));
  tempRoots.push(root);
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'repo', private: true }),
    'apps/web/tsconfig.json': JSON.stringify(ANCESTOR_CONFIG, null, 2),
    'apps/web/shared/profile.ts':
      'export interface Profile { handle: string; bio: string | null }\n',
    'apps/web/clients/app/package.json': JSON.stringify({
      name: 'app',
      private: true,
      type: 'module',
    }),
    'apps/web/clients/app/src/client.ts': CLIENT_TS,
  };
  if (serviceConfig !== undefined) {
    files[`apps/web/clients/app/${serviceConfigName}`] = JSON.stringify(serviceConfig, null, 2);
  }
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return { root, service: path.join(root, 'apps', 'web', 'clients', 'app') };
}

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: unknown[];
  }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

/** `init` scoped to the service (bounded by `scanRoot`), then one `infer`. */
async function inferProfileReturn(
  service: string,
  scanRoot: string | undefined
): Promise<{ type_string: string; any_provenance?: unknown[] }> {
  const client = new SidecarClient();
  await client.start();
  try {
    const init = await client.send<{ status: string; errors?: string[] }>({
      action: 'init',
      request_id: 'init',
      repo_root: service,
      ...(scanRoot !== undefined ? { scan_root: scanRoot } : {}),
    });
    assert.strictEqual(init.status, 'ready', JSON.stringify(init));
    const res = await client.send<InferShape>(
      {
        action: 'infer',
        request_id: 'profile',
        requests: [
          {
            file_path: path.join(service, 'src', 'client.ts'),
            line_number: PROFILE_FN_LINE,
            infer_kind: 'function_return',
            alias: 'Profile_Return',
          },
        ],
      },
      30000
    );
    const row = res.inferred_types?.find((t) => t.alias === 'Profile_Return');
    assert.ok(row, `no inference: ${JSON.stringify(res)}`);
    return row;
  } finally {
    await client.stop();
  }
}

/** One `capture_v2` of the body read, through the protocol. */
async function captureBody(
  service: string,
  scanRoot: string | undefined
): Promise<{
  result: CaptureStubResult;
  surface: string;
  snapshot: Record<string, unknown>;
  /** The stub's declaration of the shared module, when it ships one. */
  declaration: string | undefined;
}> {
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1776-stub-'));
  tempRoots.push(outDir);
  const client = new SidecarClient();
  await client.start();
  try {
    const res = await client.send<{ status: string; result?: CaptureStubResult; errors?: string[] }>(
      {
        action: 'capture_v2',
        request_id: 'capture',
        repo_root: service,
        ...(scanRoot !== undefined ? { scan_root: scanRoot } : {}),
        service_name: 'app',
        out_dir: outDir,
        anchors: [
          {
            kind: 'infer',
            alias: 'Profile_Body',
            source_file: 'src/client.ts',
            anchor_origin: 'deterministic-infer',
            line_number: PROFILE_BODY_LINE,
            expression_text: '(await res.json()) as Profile',
          },
        ],
      },
      60000
    );
    assert.ok(res.result?.success, `capture failed: ${JSON.stringify(res)}`);
    const surface = fs.readFileSync(path.join(outDir, 'types', 'surface.d.ts'), 'utf8');
    const snapshot = JSON.parse(
      fs.readFileSync(path.join(outDir, 'tsconfig.snapshot.json'), 'utf8')
    ) as Record<string, unknown>;
    const shared = path.join(outDir, 'types', '__outside__', 'shared', 'profile.d.ts');
    const declaration = fs.existsSync(shared) ? fs.readFileSync(shared, 'utf8') : undefined;
    return { result: res.result, surface, snapshot, declaration };
  } finally {
    await client.stop();
  }
}

function selfCheck(result: CaptureStubResult, alias: string): string | undefined {
  return result.aliases.find((record) => record.alias === alias)?.self_check;
}

describe("carrick#1776: the init'd project reads the nearest tsconfig above the service", () => {
  it('types a paths import under the ancestor config', async () => {
    const { root, service } = writeRepo();
    const row = await inferProfileReturn(service, root);
    assert.strictEqual(collapse(row.type_string), RESOLVED);
    assert.strictEqual(row.any_provenance, undefined);
  });

  it('searches only the service root when no scan root is named', async () => {
    const { service } = writeRepo();
    const row = await inferProfileReturn(service, undefined);
    assert.strictEqual(row.type_string, 'Profile');
    assert.ok(row.any_provenance, 'an unresolved return must carry its provenance');
  });

  it('never reads a config above the scan root', async () => {
    const { service } = writeRepo();
    // The config sits in apps/web/, one level above this bound.
    const row = await inferProfileReturn(service, path.dirname(service));
    assert.strictEqual(row.type_string, 'Profile');
    assert.ok(row.any_provenance, 'an unresolved return must carry its provenance');
  });

  it("keeps the service's own config when it has one", async () => {
    // The nearest config wins: the service's own, which states no `paths`.
    const { root, service } = writeRepo({
      include: ['src'],
      compilerOptions: { module: 'NodeNext', moduleResolution: 'NodeNext', strict: true },
    });
    const row = await inferProfileReturn(service, root);
    assert.strictEqual(row.type_string, 'Profile');
    assert.ok(row.any_provenance, 'an unresolved return must carry its provenance');
  });
});

describe('carrick#1776: capture reads the nearest tsconfig above the service', () => {
  it('emits the surface under the ancestor config', async () => {
    const { root, service } = writeRepo();
    const { result, surface, snapshot, declaration } = await captureBody(service, root);
    assert.strictEqual(selfCheck(result, 'Profile_Body'), 'ok', surface);
    // The alias names the declaration the `paths` import resolved to, which
    // ships in the stub beside the surface.
    assert.match(surface, /Profile_Body = import\("\.\/__outside__\/shared\/profile"\)\.Profile;/);
    assert.match(collapse(declaration ?? ''), /interface Profile \{ handle: string; bio: string \| null; \}/);
    assert.strictEqual(snapshot.target, 'ES2019');
  });

  it("keeps a service-root config under another name, as the init'd project does", async () => {
    // One lookup serves both readers: a service root holding only a
    // tsconfig.build.json is emitted under it, not under the ancestor.
    const { root, service } = writeRepo(
      { include: ['src'], compilerOptions: { target: 'ES2017', strict: true } },
      'tsconfig.build.json'
    );
    const { snapshot } = await captureBody(service, root);
    assert.strictEqual(snapshot.target, 'ES2017');
  });

  it('searches only the service root when no scan root is named', async () => {
    const { service } = writeRepo();
    const { result, surface, snapshot } = await captureBody(service, undefined);
    assert.notStrictEqual(selfCheck(result, 'Profile_Body'), 'ok', surface);
    assert.notStrictEqual(snapshot.target, 'ES2019');
  });
});
