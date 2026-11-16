// A tiny query layer: options objects carry a key and a fetcher, `fetchQuery`
// runs one and `useQuery` is the hook that reads one.

export type QueryOptions<T> = {
  queryKey: readonly unknown[];
  queryFn: () => Promise<T>;
};

export const queryClient = {
  fetchQuery<T>(options: QueryOptions<T>): Promise<T> {
    return options.queryFn();
  },
};

export function useQuery<T>(options: QueryOptions<T>): { data: T | undefined } {
  let data: T | undefined;
  void options.queryFn().then((value) => {
    data = value;
  });
  return { data };
}
