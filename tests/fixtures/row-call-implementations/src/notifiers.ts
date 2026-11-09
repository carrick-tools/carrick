import { http } from "./transports.js";
import type { Notifier } from "./ports.js";

export class MailNotifier implements Notifier {
  notify(e: string) {
    void http.post("/mail", e);
  }
}

export class LogNotifier implements Notifier {
  notify(e: string) {
    void http.post("/log", e);
  }
}
