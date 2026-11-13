// `deps.now || Date.now`, where no spread source writes `now`: the rule
// reads the key as absent, so the call reads as the same call with a
// literal that writes no `now` (the twin, `checkPlain`).
export type ClockDeps = { now?: () => number; limit?: number };

function checkSpread(deps: ClockDeps): boolean {
  const now = deps.now || Date.now;
  return now() > (deps.limit || 0);
}

function checkPlain(deps: ClockDeps): boolean {
  const now = deps.now || Date.now;
  return now() > (deps.limit || 0);
}

export function sweep(): boolean {
  const deps = { limit: 5 };
  return checkSpread({ ...deps, limit: 6 }) && checkPlain({ limit: 7 });
}
