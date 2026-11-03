export interface Store {
  read(key: string): Promise<number>;
}

export interface Orphan {
  ping(): void;
}
