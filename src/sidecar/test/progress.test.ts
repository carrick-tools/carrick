/**
 * The pacing of progress frames (carrick#1914).
 *
 * A long request tells the scanner it is alive by writing a `progress` frame
 * when a unit of its work finishes. A batch can hold thousands of units that
 * each take a millisecond, so the reports are paced: the first is written at
 * once, and after it at most one per interval. These tests drive the pacing
 * with a clock they control.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { atMostEvery } from '../src/progress.js';

describe('atMostEvery', () => {
  it('lets the first report through at once', () => {
    const seen: number[] = [];
    const report = atMostEvery(1500, (done: number) => seen.push(done), () => 10);
    report(1);
    assert.deepStrictEqual(seen, [1]);
  });

  it('drops reports made inside the interval and passes the next one after it', () => {
    let now = 0;
    const seen: number[] = [];
    const report = atMostEvery(1500, (done: number) => seen.push(done), () => now);
    report(1);
    now = 700;
    report(2);
    now = 1499;
    report(3);
    now = 1500;
    report(4);
    now = 1600;
    report(5);
    now = 3000;
    report(6);
    assert.deepStrictEqual(seen, [1, 4, 6]);
  });

  it('measures the interval from the last report it let through', () => {
    let now = 0;
    const seen: number[] = [];
    const report = atMostEvery(1000, (done: number) => seen.push(done), () => now);
    report(1);
    // Dropped reports do not move the mark: 1000 ms after the first is due.
    for (now = 100; now < 1000; now += 100) report(-1);
    now = 1000;
    report(2);
    assert.deepStrictEqual(seen, [1, 2]);
  });

  it('hands every argument to the report', () => {
    const seen: Array<[number, string]> = [];
    const report = atMostEvery(1, (done: number, phase: string) => seen.push([done, phase]), () => 0);
    report(3, 'infer');
    assert.deepStrictEqual(seen, [[3, 'infer']]);
  });
});
