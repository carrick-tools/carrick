import { Bus } from "@fixture/bus";

const bus = new Bus();

export function wire() {
  bus.on("cache.flushed", () => {});
  bus.emit("cache.flushed", { at: Date.now() });
}
