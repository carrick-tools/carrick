export type StreamOptions = { url: string; request?: RequestInit };

export class EventStream {
  private source: EventSource;

  constructor(private options: StreamOptions) {
    this.source = new EventSource(options.url, {
      fetch: (input: string, init?: RequestInit) => {
        return fetch(input, {
          ...init,
          ...options.request,
          headers: { Accept: "text/event-stream" },
        });
      },
    } as EventSourceInit);
  }

  stop(): void {
    this.source.close();
  }
}

export function openStream(options: StreamOptions): EventStream {
  return new EventStream(options);
}
