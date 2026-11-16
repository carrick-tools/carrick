import { useQuery } from "@example/gql-client";
import { OrderSummaryDocument } from "./generated/graphql";

export function useOrderSummary(orderId: string) {
  return useQuery(OrderSummaryDocument, { variables: { id: orderId } });
}
