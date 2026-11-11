export class Connection {
  readonly send = async (input: string, init?: RequestInit) => {
    const res = await globalThis.fetch(new URL(input, this.base), init);
    return res;
  };

  readonly sendBytes = async (input: string, init?: RequestInit) => {
    const res = await this.send(input, init);
    if (res.status === 404) {
      return null;
    }
    return await res.arrayBuffer();
  };

  constructor(private readonly base: string) {}
}
