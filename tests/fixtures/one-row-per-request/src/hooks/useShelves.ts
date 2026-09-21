import { useQuery } from "@example/query";

import { shelvesApi } from "../lib/shelves";

export function useShelves() {
  const query = useQuery({
    queryKey: ["shelves"],
    queryFn: () => shelvesApi.listMine(),
  });
  return query.data?.shelves;
}
