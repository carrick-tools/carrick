import { send } from "./send.js";

export class CatalogClient {
  constructor(private readonly baseUrl: string) {}

  readWidget<T = any>(widgetId: string): Promise<T> {
    const encoded = encodeURIComponent(widgetId);
    return send<T>(`${this.baseUrl}/api/v1/widgets/${encoded}`, {
      method: "GET",
    });
  }
}
