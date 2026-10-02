/**
 * Only the write guard touches the disk (carrick#1748).
 *
 * Every sidecar write and delete goes through `src/capture/guarded-fs.ts`, which
 * refuses a path outside the run's scratch, stub and cache roots. This test
 * reads the sidecar's sources with the compiler and fails on any other call
 * that writes, deletes or renames a file, so a new writer cannot skip the
 * guard by accident.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const srcDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../src');
const GUARD = path.join(srcDir, 'capture', 'guarded-fs.ts');

/** `fs` members that write, delete, rename, link or change a file. */
const MUTATING = new Set([
  'appendFile', 'appendFileSync', 'chmod', 'chmodSync', 'chown', 'chownSync',
  'copyFile', 'copyFileSync', 'cp', 'cpSync', 'createWriteStream',
  'futimes', 'futimesSync', 'lchmod', 'lchmodSync', 'lchown', 'lchownSync',
  'link', 'linkSync', 'lutimes', 'lutimesSync', 'mkdir', 'mkdirSync',
  'mkdtemp', 'mkdtempSync', 'open', 'openSync', 'promises', 'rename', 'renameSync',
  'rm', 'rmSync', 'rmdir', 'rmdirSync', 'symlink', 'symlinkSync',
  'truncate', 'truncateSync', 'ftruncate', 'ftruncateSync', 'unlink', 'unlinkSync',
  'utimes', 'utimesSync', 'write', 'writeSync', 'writeFile', 'writeFileSync', 'writev', 'writevSync',
]);

/** Compiler and ts-morph calls that write to disk on their own. */
const SAVING = new Set(['save', 'saveSync', 'emitSync', 'deleteImmediately', 'deleteImmediatelySync']);
/** `ts.sys` members that write. */
const SYS_WRITES = new Set(['writeFile', 'createDirectory', 'deleteFile']);

const FS_MODULES = new Set(['fs', 'node:fs', 'fs/promises', 'node:fs/promises']);

function sourceFiles(dir: string): string[] {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const abs = path.join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(abs);
    return entry.name.endsWith('.ts') && !entry.name.endsWith('.d.ts') ? [abs] : [];
  });
}

/** `file:line: text` for every raw write in one source file. */
function rawWrites(file: string): string[] {
  const text = fs.readFileSync(file, 'utf8');
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
  const fsNames = new Set<string>();
  const found: string[] = [];
  const report = (node: ts.Node, what: string) => {
    const { line } = source.getLineAndCharacterOfPosition(node.getStart(source));
    found.push(`${path.relative(srcDir, file)}:${line + 1}: ${what}`);
  };

  for (const statement of source.statements) {
    if (!ts.isImportDeclaration(statement) || !ts.isStringLiteral(statement.moduleSpecifier)) continue;
    const module = statement.moduleSpecifier.text;
    if (!FS_MODULES.has(module)) continue;
    if (module.endsWith('/promises')) report(statement, `imports ${module}`);
    const clause = statement.importClause;
    if (clause?.name) fsNames.add(clause.name.text);
    const bindings = clause?.namedBindings;
    if (bindings && ts.isNamespaceImport(bindings)) fsNames.add(bindings.name.text);
    if (bindings && ts.isNamedImports(bindings)) {
      for (const element of bindings.elements) {
        const imported = (element.propertyName ?? element.name).text;
        if (MUTATING.has(imported)) report(element, `imports ${imported} from ${module}`);
      }
    }
  }

  const visit = (node: ts.Node): void => {
    if (ts.isPropertyAccessExpression(node)) {
      const member = node.name.text;
      const target = node.expression;
      if (ts.isIdentifier(target) && fsNames.has(target.text) && MUTATING.has(member)) {
        report(node, `${target.text}.${member}`);
      }
      if (ts.isPropertyAccessExpression(target) && target.name.text === 'sys' && SYS_WRITES.has(member)) {
        report(node, `sys.${member}`);
      }
    }
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) {
      const member = node.expression.name.text;
      if (SAVING.has(member)) report(node, `.${member}()`);
      // A program emit writes its output unless it is handed a write callback.
      if (member === 'emit') {
        const callback = node.arguments[1];
        if (!callback || !(ts.isArrowFunction(callback) || ts.isFunctionExpression(callback))) {
          report(node, '.emit() without a write callback');
        }
      }
    }
    node.forEachChild(visit);
  };
  visit(source);
  return found;
}

describe('only the write guard writes to disk (carrick#1748)', () => {
  it('finds the sources it checks', () => {
    const files = sourceFiles(srcDir);
    assert.ok(files.includes(GUARD), `no ${GUARD}`);
    assert.ok(files.length > 30, `only ${files.length} source files under ${srcDir}`);
  });

  it('sees a raw write when there is one', () => {
    // The guard itself is the one file allowed to call fs directly.
    assert.ok(rawWrites(GUARD).some((line) => line.includes('fs.writeFileSync')));
  });

  it('has no write, delete or rename outside src/capture/guarded-fs.ts', () => {
    const raw = sourceFiles(srcDir)
      .filter((file) => file !== GUARD)
      .flatMap(rawWrites);
    assert.deepStrictEqual(raw, [], `route these through WriteGuard:\n${raw.join('\n')}`);
  });
});
