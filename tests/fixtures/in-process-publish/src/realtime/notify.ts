import { Subject } from "@fixture/streams";

const notices = new Subject<{ topic: string; body: unknown }>();

export function notify(topic: string, body: unknown): void {
  notices.next({ topic, body });
}
