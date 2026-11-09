import type { Mailer, Sink } from "./ports.js";
import { FileSink, QueueSink, SmtpMailer } from "./sinks.js";

// An interface member two classes implement.
export function writeAll(sink: Sink): void {
  sink.put("x");
}

// An interface member one class implements.
export function mailOne(mailer: Mailer): void {
  mailer.send("y");
}

// A member call the rows resolve: one edge, as before.
export function writeFile(): void {
  const sink = new FileSink();
  sink.put("z");
}

export function main(): void {
  writeAll(new QueueSink());
  mailOne(new SmtpMailer());
  writeFile();
}
