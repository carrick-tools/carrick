/**
 * carrick#2317: the program a capture reads its anchors in holds the files of
 * the service that declare a module or global augmentation.
 *
 * A service may declare a member on a library's type in a file its handlers
 * never import (`declare module 'lib' { interface Instance { db: Db } }` in
 * the entry file, read as `app.db` in every route file). The service's own
 * compile sees it, because the tsconfig lists the file. A program rooted at
 * the surface entry and the anchors' files does not, so the member read `any`
 * there and so did everything read off it.
 *
 * The cases:
 *  1. one project: a module augmentation and a `declare global`, each in a
 *     file no anchored file imports, with the importing handler as control;
 *  2. several owning projects: each owner's anchors see that owner's
 *     augmentations and no other project's;
 *  3. the untrusted-tree read bound: an augmentation file outside the
 *     checkout is not read, not even to look for the declaration;
 *  4. a file that declares no augmentation is not made a root.
 *
 * The Deno path is in `deno-project.test.ts`, the heap a capture holds in
 * `capture-v2-emit-program-released.test.ts`.
 */
export {};
