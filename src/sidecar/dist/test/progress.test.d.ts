/**
 * The pacing of progress frames (carrick#1914).
 *
 * A long request tells the scanner it is alive by writing a `progress` frame
 * when a unit of its work finishes. A batch can hold thousands of units that
 * each take a millisecond, so the reports are paced: the first is written at
 * once, and after it at most one per interval. These tests drive the pacing
 * with a clock they control.
 */
export {};
