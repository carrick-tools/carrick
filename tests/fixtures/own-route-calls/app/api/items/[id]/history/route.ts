import { NextResponse } from 'next/server';

type Params = { params: Promise<{ id: string }> };

export async function GET(_request: Request, { params }: Params) {
  const { id } = await params;
  return NextResponse.json({ id, events: [] });
}
