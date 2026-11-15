import { ShopService } from "./gen/services/ShopService";
export async function refundAmount(id: string): Promise<number> {
  const r = await ShopService.getRefund(id);
  return r.amount;
}
export async function returnParcels(id: string): Promise<number> {
  const r = await ShopService.getReturn(id);
  return r.parcels;
}
