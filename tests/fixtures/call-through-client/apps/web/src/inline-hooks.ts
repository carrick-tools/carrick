import { useQuery } from "@fixture/query";
import { createApi } from "./api";

/** A hook whose one callback calls a method on a factory's value (carrick#2822). */
export function useStatus() {
  return useQuery({ queryKey: ["status"], queryFn: () => createApi().status() });
}
