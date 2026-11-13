// The issue's shape in one module: `gate` is handed to `outer` as
// `{ g: gate }` and run only as `deps.g(args)`, so no call names it, though
// that call writes `log`.
export type Log = { warn(message: string): void };
export type Args = { log: Log };

function inner({ log }: Args): void {
  log.warn('x');
}

function gate(args: Args): void {
  return inner(args);
}

export function outer(deps: { g(args: Args): void }, args: Args): void {
  return deps.g(args);
}

outer({ g: gate }, { log: console });
