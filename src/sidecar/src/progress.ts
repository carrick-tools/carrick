/**
 * Pacing for the `progress` frames a long request writes (carrick#1914).
 */

/**
 * The least time between two progress frames of one request.
 *
 * The scanner's deadline for a request is how long the sidecar may stay
 * silent about it, so a frame has to say that a unit of work finished, and
 * has to be far more frequent than that deadline. It does not have to follow
 * every unit: a batch can finish thousands in a second.
 */
export const PROGRESS_INTERVAL_MS = 1500;

/**
 * `report`, held to one call per `intervalMs`: the first call goes through at
 * once, and a call made sooner than `intervalMs` after the last one that went
 * through is dropped.
 *
 * It is called by the work, between units, and never by a timer. A handler
 * that stops finishing units stops reporting, which is what the reader on the
 * other end is waiting to notice.
 */
export function atMostEvery<Args extends unknown[]>(
  intervalMs: number,
  report: (...args: Args) => void,
  now: () => number = () => performance.now()
): (...args: Args) => void {
  let last = Number.NEGATIVE_INFINITY;
  return (...args) => {
    const at = now();
    if (at - last < intervalMs) return;
    last = at;
    report(...args);
  };
}
