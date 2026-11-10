import { FetchService } from "./fetch-service";

// The service is taken from an own container by its class.
class Container {
  get<T>(cls: new (...args: never[]) => T): T {
    return new cls();
  }
}

const container = new Container();

export async function listFromContainer() {
  const fetcher = container.get(FetchService);
  return fetcher.fetch("/api/items.list", { method: "POST" });
}
