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
import { type FileSystemHost } from 'ts-morph';
/** Where every project reads the default library from: ts-morph's own folder name for it. */
export declare const DEFAULT_LIBRARY_FOLDER = "/node_modules/typescript/lib";
/** Every `lib.*.d.ts` the compiler ships, by file name. */
export declare function defaultLibraryTexts(): ReadonlyMap<string, string>;
/**
 * `inner` (the real file system when none is given) with the default
 * library's folder answered from {@link defaultLibraryTexts}. Every other
 * path goes to `inner` unchanged.
 */
export declare function withDefaultLibrary(inner?: FileSystemHost): FileSystemHost;
/**
 * The options every ts-morph project in the sidecar is built with, over
 * `fileSystem` (the real file system when none is given): the default library
 * read from the compiler's own files.
 */
export declare function defaultLibraryOptions(fileSystem?: FileSystemHost): {
    libFolderPath: string;
    fileSystem: FileSystemHost;
};
