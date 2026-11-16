// This file is generated from the API description. Do not edit it.
import { request } from "../core/request";
export type RefundDto = { id: string; amount: number; reason: string };
export type ReturnDto = { id: string; parcels: number; status: string };
export class ShopService {
  public static getRefund(refundId: string): Promise<RefundDto> {
    return request<RefundDto>({ method: "GET", url: `/api/v1/refund-old/${refundId}` });
  }
  public static getReturn(returnId: string): Promise<ReturnDto> {
    return request<ReturnDto>({ method: "GET", url: `/api/v1/returns/${returnId}` });
  }
}
