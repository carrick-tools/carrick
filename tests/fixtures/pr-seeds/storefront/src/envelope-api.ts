// A small client whose calls resolve to an envelope around the body, as many
// house clients do: the body is under `data`.

export type Envelope<T> = { data: T; status: number };

export const api = {
  async get<T>(url: string): Promise<Envelope<T>> {
    const response = await fetch(url);
    return { data: (await response.json()) as T, status: response.status };
  },
};
