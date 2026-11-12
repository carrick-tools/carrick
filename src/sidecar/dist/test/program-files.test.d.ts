/**
 * carrick#2027: a process can list the files its program was built from, in
 * order, and another process given that list builds the same program.
 *
 * A process asked about a file its tsconfig does not list adds the file to its
 * program, so the program's file order follows the order the questions came
 * in. With `stableTypeOrdering` the compiler orders two types of the same
 * name by that file order, so two processes asked the same questions in
 * different orders print a union of two same-named interfaces in different
 * orders. A pool of processes answers one pass's questions in whatever order
 * each process receives them; this is what lets every process start from the
 * same program.
 *
 * The tree: two interfaces named `Dog`, in two modules outside the program
 * (`tools/` is not in the tsconfig's `include`), and a function in a third
 * that returns a union of both under the names it imports them as. The
 * process that meets the yard's module first places it first.
 */
export {};
