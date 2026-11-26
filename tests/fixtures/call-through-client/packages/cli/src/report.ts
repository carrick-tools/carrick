/** Where reports go: supplied by the runtime. */
type Sink = (line: string) => void;

let sink: Sink | undefined;

export function useSink(next: Sink) {
  sink = next;
}

/** Hand a snapshot the caller read to the runtime's sink. */
export function report(snapshot: unknown) {
  sink?.(JSON.stringify(snapshot));
  return snapshot;
}
