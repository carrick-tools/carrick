export interface Sink {
  put(line: string): void;
}

export interface Mailer {
  send(to: string): void;
}
