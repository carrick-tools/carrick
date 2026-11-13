import { trpc } from "./client";

export function usePoll(id: string) {
  const add = trpc.shelf.add.useMutation();
  const items = trpc.shelf.drawer.items.useQuery(
    { id },
    { enabled: true },
  );
  const again = trpc.shelf.add.useMutation({
    onSuccess: () => undefined,
  });
  return { add, items, again };
}
