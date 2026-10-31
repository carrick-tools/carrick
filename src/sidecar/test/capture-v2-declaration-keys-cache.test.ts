/**
 * #3117: naming a declaration the entry exports under another name needs the
 * declaration file's own exports. The program for that file is built once per
 * file, however many names are asked of it.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { installedPackageSpecifier } from '../src/capture/installed-package.js';

function write(root: string, rel: string, text: string): void {
  const file = path.join(root, rel);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, text);
}

describe('declaration keys of an exported name', () => {
  let scratch: string;
  before(() => {
    scratch = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-decl-keys-')));
  });
  after(() => {
    fs.rmSync(scratch, { recursive: true, force: true });
  });

  it('builds one program per declaration file and keeps the exportName answers', () => {
    const pkg = path.join(scratch, 'node_modules/qcore');
    write(scratch, 'node_modules/qcore/package.json', JSON.stringify({
      name: 'qcore', version: '5.0.0',
      exports: { '.': { types: './build/index.d.ts', default: './build/index.js' } },
    }));
    write(scratch, 'package.json', JSON.stringify({ name: 'svc', version: '0.0.0', dependencies: { qcore: '5.0.0' } }));
    write(pkg, 'build/chunk-AbC123.d.ts',
      'type Orig<T> = T;\ninterface Yy<T> { (input: T): T }\ninterface Zz { z: 1 }\n' +
      'export { Orig as OmitKeys, Yy as FetchFn, Zz as Zed };\n');
    write(pkg, 'build/index.d.ts',
      "export { OmitKeys as Short, FetchFn as Fn, Zed as Zz } from './chunk-AbC123.js';\n");
    const chunk = fs.realpathSync(path.join(pkg, 'build/chunk-AbC123.d.ts'));

    // `ts.createProgram` is a getter-only export, so count what each program
    // does first: it reads its root file through `ts.sys.readFile`.
    const originalRead = ts.sys.readFile;
    let chunkReads = 0;
    ts.sys.readFile = (file: string, encoding?: string) => {
      if (file === chunk) chunkReads++;
      return originalRead(file, encoding);
    };
    let answers: Array<string | undefined>;
    try {
      const names = ['OmitKeys', 'FetchFn', 'Zed', 'OmitKeys', 'FetchFn'];
      answers = names.map((name) => installedPackageSpecifier(chunk, name)?.exportName);
    } finally {
      ts.sys.readFile = originalRead;
    }

    assert.deepStrictEqual(answers, ['Short', 'Fn', 'Zz', 'Short', 'Fn']);
    // One program for the entry (which reads the chunk as a dependency) and one
    // rooted at the chunk; a program per call would read it once per name.
    assert.strictEqual(chunkReads, 2, `chunk file read ${chunkReads} times`);
  });
});
