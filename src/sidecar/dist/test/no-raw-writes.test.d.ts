/**
 * Only the write guard touches the disk (carrick#1748).
 *
 * Every sidecar write and delete goes through `src/capture/guarded-fs.ts`, which
 * refuses a path outside the run's scratch, stub and cache roots. This test
 * reads the sidecar's sources with the compiler and fails on any other call
 * that writes, deletes or renames a file, so a new writer cannot skip the
 * guard by accident.
 */
export {};
