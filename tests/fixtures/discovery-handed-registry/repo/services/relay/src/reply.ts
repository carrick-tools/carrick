import type { WireRequest } from "fixture-wire";

export class Reply {
  constructor(private req: WireRequest) {}

  text(body: string) {
    return body.length + this.req.path.length;
  }
}
