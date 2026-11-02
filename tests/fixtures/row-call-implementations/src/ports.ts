// Declarations with no body: a call through one of these members reaches the
// implementation only when the program names exactly one.
export interface Store {
  read(k: string): Promise<unknown>;
  write(k: string, v: number): void;
  remove(k: string): void;
}
export interface Transport {
  send(body: string): Promise<number>;
}
export interface Notifier {
  notify(e: string): void;
}
export interface Procedures {
  getUser(id: string): Promise<unknown>;
}
export interface Deps {
  publish: (topic: string) => void;
  store: Store;
}
export abstract class Channel {
  abstract open(): void;
}
export interface Orphan {
  ping(): void;
}
export interface Reader {
  get(): number;
}
