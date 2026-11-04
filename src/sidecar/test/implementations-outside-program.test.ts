/**
 * carrick#3220 (R1): the `implementations` action answers over the program
 * the position walk reads, not the tsconfig's. That program holds the
 * owner's tsconfig program plus every walked file no program lists.
 *
 * The fixture's tsconfig lists `src/main.ts` only. `sink-b.ts` (a second
 * implementation of `Sink`) and `reader.ts` (a call through `Store`) are
 * walked but not listed.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { ProjectLoader } from '../src/project-loader.js';
import { ImplementationPrograms, findImplementations } from '../src/implementations.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'implementations-outside-program', 'repo');

describe('implementations: the walk\'s program (carrick#3220 R1)', () => {
  let repo: string;
  let loader: ProjectLoader;
  let walked: string[];

  const query = (file: string, text: string) => {
    const source = fs.readFileSync(path.join(repo, file), 'utf8');
    const start = source.indexOf(text);
    assert.ok(start >= 0, `${text} is in ${file}`);
    return { file: path.join(repo, file), start, end: start + text.length };
  };

  const ask = (files: string[], programs = new ImplementationPrograms()) =>
    findImplementations({
      loader,
      scanRoot: repo,
      files,
      programs,
      queries: [query('src/main.ts', 'sink.put(1)'), query('src/reader.ts', 'store.read("k")')],
    });

  before(() => {
    repo = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-impl-outside-')));
    fs.mkdirSync(path.join(repo, '.git'));
    fs.cpSync(FIXTURE, repo, { recursive: true });
    walked = fs.readdirSync(path.join(repo, 'src')).map((name) => path.join(repo, 'src', name));
    loader = new ProjectLoader({ repoRoot: repo });
    assert.strictEqual(loader.load().success, true);
  });

  after(() => {
    fs.rmSync(repo, { recursive: true, force: true });
  });

  it('answers a question in a file the tsconfig does not list, and counts an implementation in one', () => {
    const [flush, read] = ask(walked);
    assert.strictEqual(flush.reason, undefined);
    assert.deepStrictEqual(
      flush.implementations.map((i) => i.decl_file),
      ['src/sink-a.ts', 'src/sink-b.ts']
    );
    assert.strictEqual(read.reason, undefined);
    assert.strictEqual(read.declaration?.decl_file, 'src/ports.ts');
  });

  it('a file the walk does not hold is still not_in_program', () => {
    const [, read] = ask(walked.filter((file) => !file.endsWith('reader.ts')));
    assert.strictEqual(read.reason, 'not_in_program');
  });

  it('builds neither the default project nor the owner\'s kept project', () => {
    ask(walked);
    assert.strictEqual(loader.isBuilt(''), false);
    assert.strictEqual(loader.isBuilt(loader.projectKeyFor(walked[0])), false);
  });

  it('builds one program across requests with the same files, and another when they change', () => {
    const programs = new ImplementationPrograms();
    ask(walked, programs);
    ask(walked, programs);
    assert.strictEqual(programs.builds, 1);
    ask(walked.slice(1), programs);
    assert.strictEqual(programs.builds, 2);
  });

  it('releases the kept programs, and the next request builds again', () => {
    const programs = new ImplementationPrograms();
    ask(walked, programs);
    assert.strictEqual(programs.builds, 1);
    assert.strictEqual(programs.clear(), 1);
    assert.strictEqual(programs.clear(), 0);
    ask(walked, programs);
    assert.strictEqual(programs.builds, 2);
  });
});
