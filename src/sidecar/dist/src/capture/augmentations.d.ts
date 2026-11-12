/**
 * Global / module-augmentation detection (design doc, Capture step 4).
 *
 * An entry-rooted program drops `declare global` and `declare module "x"`
 * augmentation files outside the entry's import graph, and because emit
 * proceeds despite errors the tree would ship with dangling global
 * references. Capture therefore scans the tsconfig's full file list and adds
 * every augmentation-declaring file as an extra emit root.
 *
 * This deliberately over-approximates "reachable from the closure's symbols":
 * including an unrelated augmentation costs tree bytes, excluding a needed
 * one silently corrupts the closure. Cheap syntactic prefilter first, real
 * parse only on candidates.
 */
/**
 * Absolute paths of files in `fileNames` that declare global or module
 * augmentations. Files that fail to read/parse are skipped (they will fail
 * loudly elsewhere if they matter).
 */
export declare function findAugmentationFiles(fileNames: readonly string[]): string[];
