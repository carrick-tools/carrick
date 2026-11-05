export interface Sender {
  send(path: string): Promise<unknown>;
}

class FetchSender implements Sender {
  send(path: string): Promise<unknown> {
    return fetch(path);
  }
}

const live: Sender = new FetchSender();

const handlers: Record<string, () => Promise<unknown>> = {
  refresh: () => live.send("/implemented/refresh"),
};

export function run(name: string): Promise<unknown> {
  return handlers[name]();
}
