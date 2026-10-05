import { NextResponse } from 'next/server';

type Params = { params: Promise<{ id: string }> };

export async function GET(_request: Request, { params }: Params) {
  const { id } = await params;
  return NextResponse.json({ id, name: 'Widget', archived: false });
}

export async function PUT(request: Request, { params }: Params) {
  const { id } = await params;
  const body = await request.json();
  return NextResponse.json({ id, ...body });
}

export async function DELETE(_request: Request, { params }: Params) {
  const { id } = await params;
  return NextResponse.json({ id, deleted: true });
}
