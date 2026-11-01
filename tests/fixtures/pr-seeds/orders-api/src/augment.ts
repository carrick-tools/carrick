import "@fixture/queue";

declare module "@fixture/queue" {
  interface Bus {
    drain(): Promise<void>;
  }
}
