/**
 * An `infer` answer is kept per service and served again while the program it
 * was read in is the same program (carrick#2843).
 *
 * Pinned over stdio, each step in a fresh process as a scan runs it:
 * - the second process asked the same request of the same tree is served the
 *   kept answer, identical to the first, and holds the same program after;
 * - a declaration file the program reads that changed is read again;
 * - a file that now resolves in place of another is read again;
 * - a file outside the program that changed does not cost the answer;
 * - a different extraction config is read again;
 * - with the cache off nothing is kept.
 */
export {};
