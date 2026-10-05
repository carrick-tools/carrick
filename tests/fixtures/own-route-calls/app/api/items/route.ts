import { NextResponse } from 'next/server';

export async function GET() {
  return NextResponse.json({ items: [{ id: '1', name: 'Widget', archived: false }] });
}

export async function POST(request: Request) {
  const body = await request.json();
  return NextResponse.json({ id: '2', ...body }, { status: 201 });
}
