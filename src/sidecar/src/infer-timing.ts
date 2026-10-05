/**
 * Which requests of an `infer` batch were slow (carrick#1985).
 *
 * A pass can send thousands of requests and spend most of its time in a few
 * of them. The inferrer times every request; this module turns those timings
 * into what the batch's answer carries: how many there were and what they
 * add up to, the slowest few by name, the request that printed the longest
 * type, and the requests that were the first asked of their file, apart.
 *
 * It measures and names. It changes no answer and holds no type text.
 */

import type { InferSlotTiming, InferTiming } from './types.js';

/**
 * How many of its slowest requests a batch's answer names.
 *
 * Enough for a reader adding batches up to say what the slowest hundredth of
 * a whole pass took: a pass's slowest requests are among its batches'
 * slowest, and the last one a batch names is the most any it left out can
 * have taken.
 */
export const SLOWEST_SLOTS = 25;

/** Milliseconds to the microsecond: sums of these stay readable. */
const rounded = (ms: number): number => Math.round(ms * 1000) / 1000;

/** How long a request that started at `startedMs` has taken by `nowMs`. */
export function elapsedMs(startedMs: number, nowMs: number): number {
  return rounded(nowMs - startedMs);
}

/**
 * The `count` slowest of `slots`, slowest first. Requests that took the same
 * time keep the order they are in.
 */
function slowestOf(slots: readonly InferSlotTiming[], count: number): InferSlotTiming[] {
  return [...slots].sort((a, b) => b.ms - a.ms).slice(0, count);
}

/**
 * The one of `slots` that printed the longest type, the earliest of those
 * that tie; undefined when none printed anything.
 */
function longestPrintedOf(slots: readonly InferSlotTiming[]): InferSlotTiming | undefined {
  let longest: InferSlotTiming | undefined;
  for (const slot of slots) {
    if (slot.printed_length > (longest?.printed_length ?? 0)) longest = slot;
  }
  return longest;
}

/**
 * What a batch's answer says of the time its requests took. `slots` holds one
 * timing per request, in the order the batch was done with them.
 */
export function inferTiming(slots: readonly InferSlotTiming[]): InferTiming {
  let slotsMs = 0;
  let firstInFile = 0;
  let firstInFileMs = 0;
  for (const slot of slots) {
    slotsMs += slot.ms;
    if (slot.first_in_file) {
      firstInFile += 1;
      firstInFileMs += slot.ms;
    }
  }
  const longest = longestPrintedOf(slots);
  return {
    slots: slots.length,
    slots_ms: rounded(slotsMs),
    first_in_file_slots: firstInFile,
    first_in_file_ms: rounded(firstInFileMs),
    slowest: slowestOf(slots, SLOWEST_SLOTS),
    ...(longest ? { longest_printed: longest } : {}),
  };
}

/**
 * One timing for a request several projects answered, each with its own:
 * the counts and the times added up, and the slowest of all of them.
 */
export function mergeInferTimings(parts: readonly InferTiming[]): InferTiming {
  if (parts.length === 1) return parts[0];
  const sum = (read: (part: InferTiming) => number): number =>
    parts.reduce((total, part) => total + read(part), 0);
  const longest = longestPrintedOf(parts.flatMap((part) => part.longest_printed ?? []));
  return {
    slots: sum((part) => part.slots),
    slots_ms: rounded(sum((part) => part.slots_ms)),
    first_in_file_slots: sum((part) => part.first_in_file_slots),
    first_in_file_ms: rounded(sum((part) => part.first_in_file_ms)),
    slowest: slowestOf(
      parts.flatMap((part) => part.slowest),
      SLOWEST_SLOTS
    ),
    ...(longest ? { longest_printed: longest } : {}),
  };
}
