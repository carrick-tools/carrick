import { route as claimRoute } from "./routes/claim";
import { route as releaseRoute } from "./routes/release";
import { PING_URL } from "./routes/urls";

/** A route as the route modules declare one. */
export type Route = { path: string; method: string };

/** How the queue's owner sends a request: supplied at construction. */
export type Dispatch = (base: string, route: Route, props: unknown) => Promise<unknown>;

/** How the queue's owner reads a path. */
export type Get = (url: string) => Promise<unknown>;

/** A queue client whose requests go through the functions it is handed. */
export class JobQueue {
  constructor(
    private readonly base: string,
    private readonly dispatch: Dispatch,
    private readonly get: Get,
  ) {}

  call(route: Route) {
    return (props: unknown) => this.dispatch(this.base, route, props);
  }

  claim(limit: number) {
    return this.call(claimRoute)({ body: { limit } });
  }

  release(id: string) {
    return this.call(releaseRoute)({ body: { id } });
  }

  ping() {
    return this.get(PING_URL);
  }
}
