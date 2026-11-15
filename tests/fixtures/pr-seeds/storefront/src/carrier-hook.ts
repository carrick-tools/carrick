import { useQuery } from "./query-kit.js";

type Carrier = { id: string; rating: number; name: string };

export function useCarrierRating(carrierId: string): number | undefined {
  const { data } = useQuery<Carrier>({
    queryKey: ["carrier", carrierId],
    queryFn: async () => {
      const response = await fetch(`/api/v1/carriers/${carrierId}`);
      return (await response.json()) as Carrier;
    },
  });
  return data?.rating;
}
