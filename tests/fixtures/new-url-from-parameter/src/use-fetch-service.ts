import { FetchService } from "./fetch-service";

// The service is held through a constructor parameter property.
export class ItemsApi {
  constructor(private readonly fetcher: FetchService) {}

  async list() {
    return this.fetcher.fetch("/api/items.list", { method: "POST" });
  }
}
