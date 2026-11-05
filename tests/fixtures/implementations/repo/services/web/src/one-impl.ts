export interface Notifier {
  notify(path: string): Promise<unknown>;
}

class HttpNotifier implements Notifier {
  notify(path: string): Promise<unknown> {
    return fetch(path, { method: "POST" });
  }
}

const notifier: Notifier = new HttpNotifier();

export function pingOnce(): Promise<unknown> {
  return notifier.notify("/implemented/one");
}
