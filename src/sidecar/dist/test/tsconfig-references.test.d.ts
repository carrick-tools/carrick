/**
 * carrick#1604: a service whose tsconfig lists no files of its own and only
 * `references` other projects (a "solution" config) was typed with no
 * compiler options at all.
 *
 * A solution config has no `compilerOptions`; the options live in the
 * projects it references. Both readers of the service's tsconfig parsed the
 * named file alone, so `customConditions`, `paths` and NodeNext resolution
 * never applied. A workspace import that only resolves to source through a
 * custom export condition fell through to unbuilt `dist` types, and every
 * consumer type that went through it read `unknown`.
 *
 * TypeScript's editor answers "which project types this file" by searching the
 * named config, then its references depth-first in declared order, for the
 * first project whose file list includes the file. That project owns the
 * file, and a file is only ever typed under its owner's options, on both
 * readers:
 *  - capture (`captureStub`), the served surface: each anchor is resolved in
 *    its owner's program;
 *  - the init'd project (`infer`, `bundle`, `retype_check`): each request is
 *    answered by its file's owner's program, built on first use.
 *
 * Which project has more files never decides how a file is typed: a test
 * project with more files than the source project cannot type a source file,
 * and a file two projects include is typed by the first one on both readers.
 */
export {};
