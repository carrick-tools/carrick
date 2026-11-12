/**
 * The capture self-check skips type-checking TypeScript's default lib files,
 * and only those (carrick#2158).
 *
 * Checking `lib.d.ts` (DOM included) was most of a capture's self-check time.
 * `skipDefaultLibCheck: true` drops it; `skipLibCheck: false` keeps every
 * stub `.d.ts` checked, which is what makes the gate mean anything.
 *
 * The flag could hide a diagnostic the self-check reads only if the compiler
 * reported it from inside a default lib file. The self-check reads resolution
 * errors (2307/2792, `Cannot find name`), and the place they could move is a
 * stub that augments a global lib interface: the interface's first
 * declaration is in the lib. They stay at the stub.
 *
 * The judge's `tsc` keeps checking the lib: there the flag would drop a
 * stub-located TS2411 the judge counts, so its tsconfig must not carry it.
 */
export {};
