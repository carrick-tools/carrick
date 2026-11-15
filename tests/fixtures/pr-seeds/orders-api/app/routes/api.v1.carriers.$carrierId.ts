// GET /api/v1/carriers/:carrierId, from the file name and the exported loader.
export type Carrier = {
  id: string;
  rating: number;
  name: string;
};

export async function loader({
  params,
}: {
  params: { carrierId: string };
}): Promise<Carrier> {
  return { id: String(params.carrierId), rating: 4, name: "Fleet" };
}
