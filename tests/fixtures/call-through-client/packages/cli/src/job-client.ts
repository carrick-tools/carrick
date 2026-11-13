import { send } from "@fixture/client";

/** A client whose methods hand a path they are given to `send`. */
export class JobClient {
  constructor(private readonly base: string) {}

  close(path: string) {
    return send(this.base, path);
  }
}
