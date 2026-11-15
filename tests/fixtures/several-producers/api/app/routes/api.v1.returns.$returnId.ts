// GET /api/v1/returns/:returnId
export type Return = { id: string; parcels: number; status: string };
export async function loader({ params }: { params: { returnId: string } }): Promise<Return> {
  return { id: String(params.returnId), parcels: 2, status: "open" };
}
