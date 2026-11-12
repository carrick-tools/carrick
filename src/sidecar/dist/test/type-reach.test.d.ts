/**
 * Which files an edit reaches (carrick#3100).
 *
 * Each case builds a small tree, reads its import graph from a real program,
 * and asserts exactly the files `reach` answers. Every answer is also taken
 * from the live graph (`affectedFiles`) and must equal the answer over the
 * serialised graph: dropping statement texts and unrelated identifiers loses
 * nothing the walk reads.
 */
export {};
