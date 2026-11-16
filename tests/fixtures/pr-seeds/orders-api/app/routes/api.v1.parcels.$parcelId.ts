// GET /api/v1/parcels/:parcelId, from the file name and the exported loader.
export type Parcel = {
  id: string;
  weight: number;
  carrier: string;
};

export async function loader({
  params,
}: {
  params: { parcelId: string };
}): Promise<Parcel> {
  return { id: String(params.parcelId), weight: 420, carrier: "post" };
}
