import type { Sink } from "./ports";

export class SinkA implements Sink {
  put(value: number): void {
    void value;
  }
}
