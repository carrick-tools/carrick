import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { ProjectLoader } from '../src/project-loader.js';
import { captureStub, runCheck } from '../src/capture/index.js';
import ts from 'typescript';
import { DenoProject, findDenoConfig } from '../src/capture/index.js';
import { SidecarClient } from './helpers.js';
import { TypeInferrer, isExternalOrigin } from '../src/type-inferrer.js';

const hasDeno = spawnSync('deno', ['--version']).status === 0;

describe('Deno project resolution', { skip: !hasDeno }, () => {
  it('uses Deno type overrides and exact npm aliases without executing dependencies', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-registry-'));
    try {
      fs.writeFileSync(path.join(root, 'mod.js'), 'throw new Error("must never run");');
      fs.writeFileSync(path.join(root, 'types.d.ts'), 'export interface Remote { status: "remote" }; export const remote: Remote;');
      fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({
        imports: { remote: './mod.js', renamed: 'npm:zod@4.0.17', nodeTypes: 'npm:@types/node@22.15.30' },
        nodeModulesDir: 'auto', allowScripts: ['npm:zod'],
      }));
      fs.writeFileSync(path.join(root, 'main.ts'), '// @deno-types="./types.d.ts"\nimport { remote } from "remote"; import { z } from "renamed"; export const value = { remote, local: z.object({ count: z.number() }).parse({count: 1}) }; export type Value = typeof value; export type Schema = z.ZodString;');
      fs.writeFileSync(path.join(root, 'dependency.ts'), 'import type { Buffer } from "nodeTypes/buffer"; export type Bytes = Buffer;');
      const installed = spawnSync('deno', ['install', '--node-modules-dir=none'], { cwd: root, encoding: 'utf8' });
      assert.equal(installed.status, 0, installed.stderr);
      assert.equal(fs.existsSync(path.join(root, 'node_modules')), false);
      const loader = new ProjectLoader({ repoRoot: root });
      assert.equal(loader.load().success, true);
      const source = loader.getProject().getSourceFileOrThrow(path.join(root, 'main.ts'));
      const type = source.getVariableDeclarationOrThrow('value').getType();
      assert.equal(type.getPropertyOrThrow('local').getTypeAtLocation(source).getPropertyOrThrow('count').getTypeAtLocation(source).getText(), 'number');
      assert.equal(type.getPropertyOrThrow('remote').getTypeAtLocation(source).getPropertyOrThrow('status').getTypeAtLocation(source).getText(), '"remote"');
      const captured = captureStub({ repoRoot: root, serviceName: 'remote', outDir: path.join(root, '.carrick/stub'), anchors: [{ kind: 'symbol', alias: 'Value', source_file: 'main.ts', symbol_name: 'Value', anchor_origin: 'llm-symbol' }, { kind: 'symbol', alias: 'Schema', source_file: 'main.ts', symbol_name: 'Schema', anchor_origin: 'llm-symbol' }] });
      assert.equal(captured.success, true, captured.errors.join('\n'));
      assert.equal(captured.aliases[0].self_check, 'ok', JSON.stringify(captured.aliases));
      assert.equal(captured.pinned_dependencies.zod, '4.0.17');
      const tree = captured.emitted_files.map(file => fs.readFileSync(path.join(captured.stub_dir, file), 'utf8')).join('\n');
      assert.doesNotMatch(tree, /from ["'](?:remote|renamed)["']/);
      assert.match(tree, /from "zod"/);
      assert.equal(captured.bare_checkout, false);
      const deno = new DenoProject(findDenoConfig(root)!, root);
      const nodeBuffer = deno.resolve('nodeTypes/buffer', path.join(root, 'dependency.ts'), deno.parsed.options);
      assert.ok(nodeBuffer?.resolvedFileName.endsWith('buffer.d.ts'));
      const globals = path.join(path.dirname(nodeBuffer!.resolvedFileName), 'globals.d.ts');
      assert.ok(deno.resolve('undici-types', globals, deno.parsed.options)?.resolvedFileName.endsWith('index.d.ts'));
      assert.equal(fs.existsSync(path.join(root, 'node_modules')), false);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('resolves JSX type packages from the selected graph scope when npm has multiple type versions', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-jsx-'));
    try {
      fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({ imports: {
        react: 'npm:react@19.2.3', '@types/react': 'npm:@types/react@19.2.7',
        otherTypes: 'npm:@types/react@19.0.10',
      }, compilerOptions: { jsx: 'react-jsx', jsxImportSource: 'react', jsxImportSourceTypes: '@types/react' } }));
      fs.writeFileSync(path.join(root, 'main.tsx'), 'import { useState } from "react"; export function example() { return useState<number>(1)[0]; } export type Style = Pick<import("react").CSSProperties, "color">;');
      const installed = spawnSync('deno', ['install', '--node-modules-dir=none'], { cwd: root, encoding: 'utf8' });
      assert.equal(installed.status, 0, installed.stderr);
      const loader = new ProjectLoader({ repoRoot: root });
      assert.equal(loader.load().success, true);
      const source = loader.getProject().getSourceFileOrThrow(path.join(root, 'main.tsx'));
      assert.equal(source.getFunctionOrThrow('example').getReturnType().getText(), 'number');
      assert.deepEqual(loader.getProject().getPreEmitDiagnostics().map(d => d.getCode()), []);
      const captured = captureStub({ repoRoot: root, serviceName: 'jsx', outDir: path.join(root, '.carrick/stub'),
        anchors: [{ kind: 'handler_return', alias: 'Value', source_file: 'main.tsx', symbol_name: 'example', anchor_origin: 'llm-symbol' }, { kind: 'symbol', alias: 'Style', source_file: 'main.tsx', symbol_name: 'Style', anchor_origin: 'llm-symbol' }],
      });
      assert.equal(captured.success, true, captured.errors.join('\n'));
      assert.equal(captured.aliases[0].self_check, 'ok');
      assert.ok(captured.aliases.every(a => a.self_check === 'ok'), JSON.stringify(captured.aliases));
      assert.equal(captured.pinned_dependencies['@types/react'], '19.2.7');
      assert.equal(captured.pinned_dependencies.react, '19.2.3');
      const emitted = fs.readFileSync(path.join(captured.stub_dir, 'types/main.d.ts'), 'utf8');
      assert.match(emitted, /example\(\): number/);
      assert.doesNotMatch(emitted, /import\(["']@types\//);
      const consumer = path.join(root, '.carrick/consumer');
      fs.mkdirSync(path.join(consumer, 'types'), { recursive: true });
      fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ name: '@carrick/consumer', version: '0.0.0', types: 'types/surface.d.ts' }));
      fs.writeFileSync(path.join(consumer, 'types/surface.d.ts'), 'export type Style = { color?: string };');
      const checked = await runCheck({
        stubs: [{ service_name: 'jsx', stub_dir: captured.stub_dir }, { service_name: 'consumer', stub_dir: consumer }],
        pairs: [{ pair_key: 'style', protocol: 'http', type_kind: 'response', producer: { service_name: 'jsx', alias: 'Style' }, consumer: { service_name: 'consumer', alias: 'Style' } }],
      });
      assert.deepEqual(checked.verdicts.map(v => [v.bucket, v.resolved]), [['compatible', true]], JSON.stringify(checked));
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  it('keeps missing modules unresolved and preserves explicit npm tsconfigs', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-missing-'));
    try {
      fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({ imports: { missing: './absent.ts' }, compilerOptions: { lib: ['deno.ns', 'dom'] } }));
      fs.writeFileSync(path.join(root, 'main.ts'), 'import { value } from "missing"; export const result = value;');
      const deno = new DenoProject(findDenoConfig(root)!, root);
      assert.equal(deno.resolve('missing', path.join(root, 'main.ts'), deno.parsed.options), undefined);
      assert.ok(deno.diagnostics.some(d => d.includes('absent.ts')));
      fs.writeFileSync(path.join(root, 'tsconfig.json'), JSON.stringify({ compilerOptions: { strict: true, target: 'ESNext' } }));
      assert.equal(findDenoConfig(root, 'tsconfig.json'), undefined);
      fs.mkdirSync(path.join(root, 'other'));
      fs.writeFileSync(path.join(root, 'other/deno.json'), '{}');
      assert.throws(() => findDenoConfig(root, 'other/deno.json'), /Explicit Deno config must be the nearest/);
      const inferred = new ProjectLoader({ repoRoot: root });
      assert.equal(inferred.load().success, true);
      assert.equal(inferred.getProject().getSourceFiles().some(f => f.getBaseName() === 'runtime.d.ts'), true);
      const loader = new ProjectLoader({ repoRoot: root, tsconfigPath: 'tsconfig.json' });
      assert.equal(loader.load().success, true);
      assert.equal(loader.getProject().getSourceFiles().some(f => f.getBaseName() === 'runtime.d.ts'), false);
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  it('uses the same explicit TypeScript selection for infer and capture in a mixed service', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-selection-'));
    try {
      fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({ imports: { choice: './deno-value.ts' } }));
      fs.writeFileSync(path.join(root, 'tsconfig.json'), JSON.stringify({ compilerOptions: {
        strict: true, target: 'ESNext', moduleResolution: 'Bundler', module: 'ESNext',
        paths: { choice: ['./ts-value.ts'] },
      } }));
      fs.writeFileSync(path.join(root, 'deno-value.ts'), 'export const choice: number = 1;');
      fs.writeFileSync(path.join(root, 'ts-value.ts'), 'export const choice: string = "one";');
      fs.writeFileSync(path.join(root, 'main.ts'), 'import { choice } from "choice"; export const value = { choice }; export type Value = typeof value;');
      for (const tsconfigPath of [undefined, 'tsconfig.json']) {
        const expected = tsconfigPath ? 'string' : 'number';
        const loader = new ProjectLoader({ repoRoot: root, tsconfigPath });
        assert.equal(loader.load().success, true);
        const source = loader.getProject().getSourceFileOrThrow(path.join(root, 'main.ts'));
        assert.equal(source.getVariableDeclarationOrThrow('value').getType().getPropertyOrThrow('choice').getTypeAtLocation(source).getText(), expected);
        const captured = captureStub({ repoRoot: root, tsconfigPath, serviceName: 'mixed',
          outDir: path.join(root, '.carrick', expected),
          anchors: [{ kind: 'symbol', alias: 'Value', source_file: 'main.ts', symbol_name: 'Value', anchor_origin: 'llm-symbol' }],
        });
        assert.equal(captured.success, true, captured.errors.join('\n'));
        assert.equal(captured.aliases[0].self_check, 'ok');
        const surface = path.join(captured.stub_dir, 'types/surface.d.ts');
        const program = ts.createProgram([surface], { strict: true, moduleResolution: ts.ModuleResolutionKind.Bundler, module: ts.ModuleKind.ESNext });
        const checker = program.getTypeChecker();
        const alias = program.getSourceFile(surface)!.statements.find(ts.isTypeAliasDeclaration)!;
        const prop = checker.getTypeAtLocation(alias).getProperty('choice')!;
        assert.equal(checker.typeToString(checker.getTypeOfSymbolAtLocation(prop, alias)), expected);
      }
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  it('shares root aliases, member scopes, workspace exports and globals with capture', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-'));
    const write = (file: string, text: string): void => {
      fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
      fs.writeFileSync(path.join(root, file), text);
    };
    try {
      write('deno.jsonc', `{
        // targets belong to this config
        "workspace": ["./app", "./lib"],
        "imports": { "@root/": "./shared/" },
        "compilerOptions": { "types": ["./global.d.ts"] },
      }`);
      write('global.d.ts', 'interface DomainGlobal { status: "configured" }');
      write('shared/model.ts', 'export interface Model { id: string; amount: number }');
      write('lib/deno.json', JSON.stringify({ name: '@sample/lib', version: '1.0.0', exports: './mod.ts', imports: { '@local': './value.ts' } }));
      write('lib/value.ts', 'export const value = 123;');
      write('lib/mod.ts', 'import { value } from "@local"; export const remote = value;');
      write('app/deno.json', JSON.stringify({ imports: { '@local': './value.ts' } }));
      write('app/value.ts', 'export const value = "local";');
      write('app/main.ts', `import type { Model } from '@root/model.ts';
import { remote } from '@sample/lib';
import { value } from '@local';
export const env = Deno.env.get('URL');
export type Environment = typeof env;
export function response(): Model { return { id: value, amount: remote }; }
export const number = remote;
export const text = value;
export type Options = Deno.OpenOptions;
export async function openFile() { return await Deno.open('example.txt'); }
export function webResponse() { return new Response('ok'); }
export type Bytes = Uint8Array;
export type Configured = DomainGlobal;
`);
      const service = path.join(root, 'app');
      const loader = new ProjectLoader({ repoRoot: service });
      assert.equal(loader.load().success, true);
      const project = loader.getProject();
      const source = project.getSourceFileOrThrow(path.join(service, 'main.ts'));
      assert.equal(source.getVariableDeclarationOrThrow('env').getType().getText(), 'string | undefined');
      assert.equal(source.getVariableDeclarationOrThrow('number').getType().getText(), '123');
      assert.equal(source.getVariableDeclarationOrThrow('text').getType().getText(), '"local"');
      const missing = project.getPreEmitDiagnostics().filter(d => [2307, 2503, 2304].includes(d.getCode()));
      assert.deepEqual(missing.map(d => d.getMessageText()), []);
      const result = captureStub({ repoRoot: service, serviceName: 'app', outDir: path.join(root, '.carrick/stub'), anchors: [
        { kind: 'handler_return', alias: 'Response', symbol_name: 'response', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'Environment', symbol_name: 'Environment', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'Options', symbol_name: 'Options', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
        { kind: 'handler_return', alias: 'File', symbol_name: 'openFile', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
        { kind: 'handler_return', alias: 'WebResponse', symbol_name: 'webResponse', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'Bytes', symbol_name: 'Bytes', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'Configured', symbol_name: 'Configured', source_file: 'main.ts', anchor_origin: 'llm-symbol' },
      ] });
      assert.equal(result.success, true, result.errors.join('\n'));
      assert.ok(result.aliases.every(a => a.self_check === 'ok'), JSON.stringify(result.aliases));
      const surface = path.join(result.stub_dir, 'types/surface.d.ts');
      const program = ts.createProgram([surface], { strict: true, noEmit: true, skipLibCheck: true, moduleResolution: ts.ModuleResolutionKind.Bundler, module: ts.ModuleKind.ESNext });
      const checker = program.getTypeChecker();
      const sf = program.getSourceFile(surface)!;
      const alias = sf.statements.find(s => ts.isTypeAliasDeclaration(s) && s.name.text === 'Response')!;
      const type = checker.getTypeAtLocation(alias);
      assert.deepEqual(type.getProperties().map(p => p.name).sort(), ['amount', 'id']);
      const strictProgram = ts.createProgram([surface], { strict: true, noEmit: true, target: ts.ScriptTarget.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler, module: ts.ModuleKind.ESNext });
      assert.deepEqual(ts.getPreEmitDiagnostics(strictProgram).map(d => `${d.file?.fileName}:${d.code}: ${ts.flattenDiagnosticMessageText(d.messageText, ' ')}`), []);
      write('consumer/types/surface.d.ts', 'export type Expected = { id: string; amount: number };\nexport type Wrong = { id: number; amount: number };\nexport type Options = { read?: boolean };\nexport type File = { read(buffer: Uint8Array): Promise<number | null> };\nexport type WebResponse = { status: number; ok: boolean };');
      write('consumer/package.json', JSON.stringify({ name: '@carrick/consumer', version: '0.0.0', types: './types/surface.d.ts' }));
      const checked = await runCheck({
        stubs: [{ service_name: 'app', stub_dir: result.stub_dir }, { service_name: 'consumer', stub_dir: path.join(root, 'consumer') }],
        pairs: ['Expected', 'Wrong', 'Options', 'File', 'WebResponse'].map(alias => ({ pair_key: alias, protocol: 'http', type_kind: 'response', producer: { service_name: 'app', alias: ['Options', 'File', 'WebResponse'].includes(alias) ? alias : 'Response' }, consumer: { service_name: 'consumer', alias } })),
      });
      assert.deepEqual(checked.verdicts.sort((a, b) => a.pair_key.localeCompare(b.pair_key)).map(v => [v.bucket, v.resolved]), [['compatible', true], ['compatible', true], ['compatible', true], ['compatible', true], ['incompatible', true]], JSON.stringify(checked));
      assert.equal(fs.existsSync(path.join(service, 'package.json')), false);
      assert.equal(fs.existsSync(path.join(service, 'tsconfig.json')), false);
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });

  /**
   * carrick#1260: a Deno service resolves npm types out of the Deno cache
   * (`$DENO_DIR/npm/<registry-host>/<name>/<version>/...`), a path that holds
   * no `node_modules` segment. The receiver-type answer that lets the driver
   * classify a bare `x.verb("/lit", arg)` site carries the package that
   * DECLARES the receiver's type, and reading that name off the path found
   * none for every dependency-typed receiver in a Deno repo — so no site in
   * such a service could be classified at all, however well its type resolved.
   *
   * The graph knows which package owns a file; the path shape is not consulted.
   */
  it('names the package that declares a receiver type resolved through the Deno graph, npm or JSR', async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-receiver-'));
    const client = new SidecarClient();
    const main = `import { z } from "zod";
import { encodeHex } from "@std/encoding/hex";

const schema = z.object({ id: z.string() });
const local = { parse(value: unknown): unknown { return value; } };

export const parsed = schema.parse({ id: "1" });
export const untouched = local.parse({ id: "1" });
export const hex = encodeHex(new Uint8Array());
`;
    const spanOf = (marker: string) => {
      const start = main.indexOf(marker);
      assert.ok(start >= 0, `fixture must contain: ${marker}`);
      return {
        span_start: start,
        span_end: start + marker.length,
        line_number: main.slice(0, start).split('\n').length,
      };
    };
    try {
      fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({
        imports: { zod: 'npm:zod@4.0.17', '@std/encoding': 'jsr:@std/encoding@1.0.10' },
      }));
      fs.writeFileSync(path.join(root, 'main.ts'), main);
      const installed = spawnSync('deno', ['install', '--node-modules-dir=none'], { cwd: root, encoding: 'utf8' });
      assert.equal(installed.status, 0, installed.stderr);
      assert.equal(fs.existsSync(path.join(root, 'node_modules')), false);
      const mainPath = path.join(root, 'main.ts');
      await client.start();
      await client.send({ action: 'init', request_id: 'init', repo_root: root });
      const response = (await client.send({
        action: 'infer',
        request_id: 'receivers',
        requests: [
          { file_path: mainPath, ...spanOf('schema.parse({ id: "1" })'), infer_kind: 'receiver_type', alias: 'npm_site' },
          { file_path: mainPath, ...spanOf('local.parse({ id: "1" })'), infer_kind: 'receiver_type', alias: 'workspace_site' },
        ],
      })) as { status: string; errors?: string[]; inferred_types?: Array<{ alias: string; type_string: string; declaring_package?: string }> };
      assert.equal(response.status, 'success', JSON.stringify(response.errors));
      const byAlias = new Map((response.inferred_types ?? []).map(entry => [entry.alias, entry]));
      const npmSite = byAlias.get('npm_site');
      assert.ok(npmSite, 'the npm-typed receiver produced an answer');
      assert.equal(npmSite.declaring_package, 'zod');
      // A type the service itself declares names no package, in a Deno project
      // exactly as in a Node one.
      assert.equal(byAlias.get('workspace_site')?.declaring_package, undefined);

      // A JSR module's cached copy is content-addressed, so only its specifier
      // carries the package it belongs to.
      const deno = new DenoProject(findDenoConfig(root)!, root);
      const hex = deno.resolve('@std/encoding/hex', mainPath, deno.parsed.options);
      assert.ok(hex, 'the JSR module resolved');
      assert.equal(deno.packageOf(hex.resolvedFileName), '@std/encoding');
      assert.equal(deno.packageOf(mainPath), undefined);
    } finally {
      await client.stop();
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('reads a dependency type served from the Deno npm cache as machinery origin (carrick#1264)', () => {
    // No `node_modules` segment anywhere in the declaring path: the gate can
    // only answer from the compiler's own record of the graph's verdict.
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-origin-npm-'));
    try {
      fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({ imports: { hono: 'npm:hono@4.12.12' } }));
      fs.writeFileSync(path.join(root, 'main.ts'), [
        'import type { HonoRequest } from "hono";',
        'export interface Payload { id: string }',
        'export function envelope(): HonoRequest { return null as unknown as HonoRequest; }',
        'export function payload(): Payload { return { id: "x" }; }',
        'export const sent = envelope();',
        'export const body = payload();',
      ].join('\n'));
      const installed = spawnSync('deno', ['install', '--node-modules-dir=none'], { cwd: root, encoding: 'utf8' });
      assert.equal(installed.status, 0, installed.stderr);
      const loader = new ProjectLoader({ repoRoot: root });
      assert.equal(loader.load().success, true);
      const project = loader.getProject();
      const main = project.getSourceFileOrThrow(path.join(root, 'main.ts'));
      const declaring = main.getFunctionOrThrow('envelope').getReturnType().getSymbolOrThrow().getDeclarations()[0].getSourceFile();
      assert.doesNotMatch(declaring.getFilePath(), /\/node_modules\//);
      assert.match(declaring.getFilePath(), /\/hono\/4\.12\.12\/dist\/types\/request\.d\.ts$/);
      const program = project.getProgram().compilerObject;
      assert.equal(isExternalOrigin(program, declaring.compilerNode, root), true);
      assert.equal(isExternalOrigin(program, main.compilerNode, root), false);

      // Infer: the transport wrapper abstains, the payload beside it publishes.
      const inferrer = new TypeInferrer({ project, repoRoot: root, packageOf: (file) => loader.packageOf(file) });
      const inferred = inferrer.infer([
        { file_path: 'main.ts', line_number: 3, infer_kind: 'function_return', alias: 'Envelope' },
        { file_path: 'main.ts', line_number: 4, infer_kind: 'function_return', alias: 'Body' },
      ]);
      // An abstain is no row at all (the driver pads it to `unknown`); the
      // reason is only visible in the capture record below.
      assert.deepEqual((inferred.inferred_types ?? []).map(t => [t.alias, t.type_string]), [['Body', '{ id: string; }']], JSON.stringify(inferred));
      assert.equal((inferred.errors ?? []).length, 1, JSON.stringify(inferred.errors));

      // Capture: the same gate demotes the wrapper anchor as machinery.
      const captured = captureStub({ repoRoot: root, serviceName: 'origin', outDir: path.join(root, '.carrick/stub'), anchors: [
        { kind: 'infer', alias: 'Envelope', source_file: 'main.ts', anchor_origin: 'deterministic-infer', expression_text: 'envelope()' },
        { kind: 'infer', alias: 'Body', source_file: 'main.ts', anchor_origin: 'deterministic-infer', expression_text: 'payload()' },
      ] });
      assert.equal(captured.success, true, captured.errors.join('\n'));
      const record = (alias: string) => captured.aliases.find(a => a.alias === alias)!;
      assert.match(record('Envelope').capture_failure_reason ?? '', /framework machinery/, JSON.stringify(record('Envelope')));
      assert.equal(record('Body').self_check, 'ok', record('Body').self_check_detail);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('reads a JSR module type from the copy beside the materialised runtime as machinery origin (carrick#1264)', () => {
    // A JSR dependency is copied under `.carrick/deno/<hash>/remote/`, so the
    // artefact clause answers it and the compiler's external flag stays off:
    // that flag also gates declaration emit, and the copy must still emit.
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-deno-origin-jsr-'));
    try {
      fs.writeFileSync(path.join(root, 'deno.json'), JSON.stringify({ imports: { '@std/http': 'jsr:@std/http@1' } }));
      fs.writeFileSync(path.join(root, 'main.ts'), [
        'import type { ServeDirOptions } from "@std/http/file-server";',
        'export type Options = ServeDirOptions;',
      ].join('\n'));
      const installed = spawnSync('deno', ['install', '--node-modules-dir=none'], { cwd: root, encoding: 'utf8' });
      assert.equal(installed.status, 0, installed.stderr);
      const loader = new ProjectLoader({ repoRoot: root });
      assert.equal(loader.load().success, true);
      const project = loader.getProject();
      const main = project.getSourceFileOrThrow(path.join(root, 'main.ts'));
      const declaring = main.getTypeAliasOrThrow('Options').getType().getSymbolOrThrow().getDeclarations()[0].getSourceFile();
      assert.match(declaring.getFilePath(), /\/\.carrick\/deno\/[0-9a-f]+\/remote\//);
      assert.equal(loader.packageOf(declaring.getFilePath()), '@std/http');
      const program = project.getProgram().compilerObject;
      assert.equal(program.isSourceFileFromExternalLibrary(declaring.compilerNode), false);
      assert.equal(isExternalOrigin(program, declaring.compilerNode, root), true);
      assert.equal(isExternalOrigin(program, main.compilerNode, root), false);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
});
