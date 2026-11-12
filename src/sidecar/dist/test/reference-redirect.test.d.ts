/**
 * carrick#2418: a workspace package whose output is not built is typed from
 * its source under its own project's options, as an editor types it.
 *
 * A service config that references other projects tells the compiler two
 * things an editor acts on:
 *
 *  - Rule A, the owner: a project does not own a file that one of the
 *    projects it references lists. tsserver's default-project rule.
 *  - Rule B, the references: in an owner's program, an import that lands on
 *    a referenced project's declaration output (`dist/index.d.ts`, never
 *    built) reads that project's source instead, and an import made from a
 *    referenced project's own file resolves under that project's options
 *    (its `paths`, its conditions).
 *
 * The fixture (`test/fixtures/reference-redirect/`) is materialised in a temp
 * dir with the links a workspace install writes. Its service config lists
 * every file by default include and references only `tsconfig.src.json`,
 * which references three packages. A fourth package resolves only through
 * the source project's custom export condition (Rule A alone), and a fifth
 * is never referenced, so it stays unresolved, as in the editor.
 *
 * The oracle is the compiler's own editor mode: `ts.createProgram` handed the
 * project references with `useSourceOfProjectReferenceRedirect`. The
 * loader's error-type positions on the fixture are exactly that program's.
 */
export {};
