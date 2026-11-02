import { send } from "./send.js";

export type Widget = {
  id: string;
  name: string;
  activeCount: number;
};

export class CatalogClient {
  constructor(private readonly baseUrl: string) {}

  readWidget(widgetId: string): Promise<Widget> {
    const encoded = encodeURIComponent(widgetId);
    return send<Widget>(`${this.baseUrl}/api/v1/widgets/${encoded}`, {
      method: "GET",
    });
  }
}
