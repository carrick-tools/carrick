export interface Store {
  read(key: string): number;
}

export interface Sink {
  put(value: number): void;
}
