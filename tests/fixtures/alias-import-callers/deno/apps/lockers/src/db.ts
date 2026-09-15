export interface Tx {
  write(table: string, row: unknown): void;
}

export const db = {
  async transaction<T>(fn: (tx: Tx) => Promise<T>): Promise<T> {
    return await fn({ write: () => {} });
  },
};
