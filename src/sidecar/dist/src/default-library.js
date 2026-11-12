/**
 * The TypeScript default library (`lib.*.d.ts`) every ts-morph project reads
 * (carrick#3509).
 *
 * Without `libFolderPath`, ts-morph serves its own copy of these files from
 * memory, and that copy is minified: comments are stripped and declarations
 * run together, so every member of `lib.dom.d.ts` sits on its line 3. A row's
 * `decl_line` for a library member, and the declarations a verified op names,
 * then point at one line for every member, and a label verified for one
 * member (`BroadcastChannel.addEventListener`) is matched by every other
 * (`Body.json`, `Crypto.randomUUID`).
 *
 * Every project here reads the compiler's own files instead, at the folder
 * ts-morph's copy is at (`/node_modules/typescript/lib`), so the files keep
 * the names they always had (`resolved_file`, `lib:<name>`) and only their
 * text changes. The texts come from the `typescript` package beside the
 * sidecar; the surface lister bundle carries them inlined
 * (`lister/build.mjs`), as it carried ts-morph's copy before.
 */
import { Project } from 'ts-morph';
import * as fs from 'node:fs';
import * as path from 'node:path';
import ts from 'typescript';
import { sidecarCompilerOptions } from './capture/index.js';
/** Where every project reads the default library from: ts-morph's own folder name for it. */
export const DEFAULT_LIBRARY_FOLDER = '/node_modules/typescript/lib';
/**
 * The bundle's inlined texts, by file name. `lister/build.mjs` defines this
 * one reference (an esbuild define is inlined at every reference, so there is
 * exactly one); the unbundled sidecar has no such global and reads the files
 * from disk.
 */
const BUNDLED = globalThis
    .__CARRICK_DEFAULT_LIBRARY__;
let texts;
/** Every `lib.*.d.ts` the compiler ships, by file name. */
export function defaultLibraryTexts() {
    if (texts)
        return texts;
    if (BUNDLED !== undefined) {
        texts = new Map(Object.entries(BUNDLED));
        return texts;
    }
    const folder = path.dirname(ts.getDefaultLibFilePath({}));
    texts = new Map(fs
        .readdirSync(folder)
        .filter((name) => /^lib(\..+)?\.d\.ts$/.test(name))
        .sort()
        .map((name) => [name, fs.readFileSync(path.join(folder, name), 'utf8')]));
    return texts;
}
function libraryFileOf(p) {
    const normal = p.split(path.sep).join('/');
    if (path.posix.dirname(normal) !== DEFAULT_LIBRARY_FOLDER)
        return undefined;
    return path.posix.basename(normal);
}
function isLibraryFolder(p) {
    return p.split(path.sep).join('/').replace(/\/+$/, '') === DEFAULT_LIBRARY_FOLDER;
}
/**
 * `inner` (the real file system when none is given) with the default
 * library's folder answered from {@link defaultLibraryTexts}. Every other
 * path goes to `inner` unchanged.
 */
export function withDefaultLibrary(inner) {
    const base = inner ??
        new Project({
            compilerOptions: sidecarCompilerOptions({}),
            skipAddingFilesFromTsConfig: true,
            skipFileDependencyResolution: true,
            skipLoadingLibFiles: true,
        }).getFileSystem();
    const missing = (p) => Object.assign(new Error(`ENOENT: no such file or directory, open '${p}'`), { code: 'ENOENT' });
    const read = (p) => {
        const name = libraryFileOf(p);
        return name === undefined ? undefined : defaultLibraryTexts().get(name);
    };
    const isLibraryPath = (p) => libraryFileOf(p) !== undefined || isLibraryFolder(p);
    return new Proxy(base, {
        get(target, prop, receiver) {
            const value = Reflect.get(target, prop, receiver);
            if (typeof value !== 'function')
                return value;
            const fn = value.bind(target);
            switch (prop) {
                case 'readFileSync':
                    return (p, e) => {
                        if (!isLibraryPath(p))
                            return fn(p, e);
                        const text = read(p);
                        if (text === undefined)
                            throw missing(p);
                        return text;
                    };
                case 'readFile':
                    return async (p, e) => {
                        if (!isLibraryPath(p))
                            return fn(p, e);
                        const text = read(p);
                        if (text === undefined)
                            throw missing(p);
                        return text;
                    };
                case 'fileExistsSync':
                    return (p) => (isLibraryPath(p) ? read(p) !== undefined : fn(p));
                case 'fileExists':
                    return async (p) => (isLibraryPath(p) ? read(p) !== undefined : fn(p));
                case 'directoryExistsSync':
                    return (p) => isLibraryFolder(p) || (libraryFileOf(p) === undefined && fn(p));
                case 'directoryExists':
                    return async (p) => isLibraryFolder(p) || (libraryFileOf(p) === undefined && (await fn(p)));
                case 'readDirSync':
                    return (p) => isLibraryFolder(p)
                        ? [...defaultLibraryTexts().keys()].map((name) => ({ name, isFile: true, isDirectory: false, isSymlink: false }))
                        : fn(p);
                case 'realpathSync':
                    return (p) => (isLibraryPath(p) ? p : fn(p));
                default:
                    return fn;
            }
        },
    });
}
/**
 * The options every ts-morph project in the sidecar is built with, over
 * `fileSystem` (the real file system when none is given): the default library
 * read from the compiler's own files.
 */
export function defaultLibraryOptions(fileSystem) {
    return { libFolderPath: DEFAULT_LIBRARY_FOLDER, fileSystem: withDefaultLibrary(fileSystem) };
}
