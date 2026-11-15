// GET /api/v1/returns/:returnId, from the file name and the exported loader.
export type Return = {
  id: string;
  items: number;
  status: string;
};

export async function loader({
  params,
}: {
  params: { returnId: string };
}): Promise<Return> {
  return { id: String(params.returnId), items: 2, status: "open" };
}
