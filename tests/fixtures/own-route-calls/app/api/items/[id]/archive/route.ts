import { NextResponse } from 'next/server';

type Params = { params: Promise<{ id: string }> };

export async function POST(_request: Request, { params }: Params) {
  const { id } = await params;
  return NextResponse.json({ id, archived: true });
}

export async function DELETE(_request: Request, { params }: Params) {
  const { id } = await params;
  return NextResponse.json({ id, archived: false });
}
