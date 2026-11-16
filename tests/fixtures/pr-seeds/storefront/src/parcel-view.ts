import { api } from "./envelope-api.js";

type Parcel = { id: string; weight: number; carrier: string };

export async function weightOf(parcelId: string): Promise<number> {
  const res = await api.get<Parcel>(`/api/v1/parcels/${parcelId}`);
  return res.data.weight;
}
