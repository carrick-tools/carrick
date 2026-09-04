import { mergeOptions, send } from "../send.js";

const WidgetSchema = { kind: "widget" };

export class ApiClient {
  constructor(
    private readonly baseUrl: string,
    private readonly defaults: { retries?: number } = {}
  ) {}

  retrieveWidget(id: string) {
    const encoded = encodeURIComponent(id);
    return send(
      WidgetSchema,
      `${this.baseUrl}/api/v2/widgets/${encoded}`,
      {
        method: "GET",
        headers: this.headers(),
      },
      mergeOptions(this.defaults, {})
    );
  }

  archiveWidget(id: string) {
    const encoded = encodeURIComponent(id);
    return send(
      WidgetSchema,
      `${this.baseUrl}/api/v2/widgets/${encoded}/archive`,
      {
        method: "POST",
        headers: this.headers(),
      },
      mergeOptions(this.defaults, {})
    );
  }

  // One half of a name the surface declares twice, differently.
  list() {
    return send(
      WidgetSchema,
      `${this.baseUrl}/api/v2/widgets`,
      {
        method: "GET",
        headers: this.headers(),
      },
      mergeOptions(this.defaults, {})
    );
  }

  private headers() {
    return { Accept: "application/json" };
  }
}
