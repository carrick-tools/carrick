/**
 * A capture holds none of its earlier compiler programs when the self-check
 * starts (carrick#1916).
 *
 * A capture builds three programs one after another: the one the anchors are
 * read in, the one that emits the declarations, and the self-check's, over
 * the emitted stub. Each is as large as the service, and the self-check is
 * the stage in which every capture measured to run out of heap died.
 *
 * The emit's program used to live as long as the capture. The emit ran inside
 * the capture's one long function, and that function's frame went on holding
 * what the emit had built while the self-check loaded its own program beside
 * it. On a 2,345-file service that was 1,290 MB of a 2,741 MB peak, the
 * largest single part of it, and nothing read it again.
 *
 * So the property is read where it costs: at the report a capture makes as
 * the self-check starts, after a forced collection, the heap holds what it
 * held before the first program was built, and not a program more.
 */
export {};
