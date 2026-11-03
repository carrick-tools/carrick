import type { Sink } from "./ports";

export class SinkB implements Sink {
  put(value: number): void {
    console.log(value);
  }
}
