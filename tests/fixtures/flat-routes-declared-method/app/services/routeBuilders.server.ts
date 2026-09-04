// The builders the routes are made with. Nothing in the scanner reads this
// file to decide a verb: the option is read at the call site, where the route
// module states it.
type Options = {
  params?: Record<string, string>;
  maxContentLength?: number;
  method?: string | string[];
  retry?: { method: string; attempts: number };
};

export function createWriteRoute<T>(options: Options, handler: T) {
  return { action: handler, options };
}

export function createResourceRoute<T>(options: Options, handler: T) {
  return { loader: handler, action: handler, options };
}
