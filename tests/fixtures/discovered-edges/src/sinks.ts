import type { Mailer, Sink } from "./ports.js";

export class FileSink implements Sink {
  put(line: string): void {
    console.log(line);
  }
}

export class QueueSink implements Sink {
  put(line: string): void {
    console.log(line);
  }
}

export class SmtpMailer implements Mailer {
  send(to: string): void {
    console.log(to);
  }
}
