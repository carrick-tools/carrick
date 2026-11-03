import type { Sink } from "./ports";
import { SinkA } from "./sink-a";

export function flush(sink: Sink): void {
  sink.put(1);
}

export function start(): void {
  flush(new SinkA());
}
