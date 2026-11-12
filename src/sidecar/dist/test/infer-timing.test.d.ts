/**
 * An `infer` batch says which of its requests were slow (carrick#1985).
 *
 * A pass of thousands of requests can spend most of its time in a handful of
 * them, and its answer used to say nothing about which. Every request is now
 * timed, and the answer names the slowest few beside the types: for each, the
 * request (alias, file, line, kind), its wall time, how much of that went on
 * computing the type and how much on printing it, how long the type it
 * printed is, and whether it was the first request this process was asked of
 * its file.
 *
 * Four layers are pinned:
 * - the phase clock: what is counted as computing a type and as printing one;
 * - the summary of a batch's timings: which requests it names, in what order,
 *   and what it adds up;
 * - the inferrer: every request is timed whatever it answered, its time is
 *   split, and the timing sits beside the answers, never in them;
 * - over stdio: the terminal frame carries the timing, for a request one
 *   project answers and for one two projects answer.
 *
 * Nothing here reads a clock: which request is slowest is tested on timings
 * the test writes, and the inferrer's own are only checked for being there.
 */
export {};
