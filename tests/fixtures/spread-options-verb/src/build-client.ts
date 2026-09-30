import { openStream } from "./sse.js";

export class BuildClient {
  constructor(private apiUrl: string) {}

  complete(id: string, body: { note: string }) {
    const source = openStream({
      url: `${this.apiUrl}/api/v1/builds/${id}/complete`,
      request: {
        method: "PATCH",
        body: JSON.stringify(body),
      },
    });
    return source;
  }
}
