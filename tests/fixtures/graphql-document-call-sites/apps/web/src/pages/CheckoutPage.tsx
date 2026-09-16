import { useMutation, useQuery } from '@example/gql-client';
import { OrdersDocument, PlaceOrderDocument } from '../generated/documents';

// Caisse — récapitulatif des commandes
export function CheckoutPage() {
  const [orders] = useQuery(OrdersDocument);
  const [placeOrder] = useMutation(PlaceOrderDocument);

  async function exportOrders(): Promise<unknown> {
    const res = await fetch(`${process.env.ORDERS_API_URL}/orders/export`);
    return res.json();
  }

  return { orders, placeOrder, exportOrders };
}
