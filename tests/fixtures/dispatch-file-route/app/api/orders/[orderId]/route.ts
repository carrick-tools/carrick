type OrderCommand =
  | { op: 'confirm' }
  | { op: 'cancel'; reason: string }
  | { op: 'refund'; amount: number };

type Params = { params: Promise<{ orderId: string }> };

export async function GET(_request: Request, { params }: Params) {
  const { orderId } = await params;
  return Response.json({ orderId });
}

export async function POST(request: Request, { params }: Params) {
  const { orderId } = await params;
  const command = (await request.json()) as OrderCommand;
  if (command.op === 'confirm') {
    return Response.json({ orderId, status: 'confirmed' });
  }
  if (command.op === 'cancel') {
    return Response.json({ orderId, status: 'cancelled', reason: command.reason });
  }
  if (command.op === 'refund') {
    return Response.json({ orderId, refunded: command.amount });
  }
  return Response.json({ error: 'unknown op' }, { status: 400 });
}
