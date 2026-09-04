import { mergeOptions, send } from "./send.js";

const WidgetSchema = { kind: "widget" };

// A second package publishing a member of the SAME name against a different
// route. Which package the receiver came out of is the whole of the difference.
export class OtherClient {
  constructor(private readonly baseUrl: string) {}

  retrieveWidget(id: string) {
    const encoded = encodeURIComponent(id);
    return send(
      WidgetSchema,
      `${this.baseUrl}/api/other/widgets/${encoded}`,
      {
        method: "GET",
        headers: { Accept: "application/json" },
      },
      mergeOptions({}, {})
    );
  }
}

export const otherManager = {
  clientOrThrow(): OtherClient {
    return new OtherClient(process.env.OTHER_API_URL ?? "http://localhost:3100");
  },
};
