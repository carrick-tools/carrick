import type { Transport } from "../ports.js";

// A test source by path, inside the program: it is not a candidate.
export class FakeTransport implements Transport {
  async send() {
    return 0;
  }
}
