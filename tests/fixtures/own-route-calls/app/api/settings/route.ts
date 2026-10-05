import { NextResponse } from 'next/server';

export async function GET() {
  return NextResponse.json({ currency: 'EUR' });
}

export async function PUT(request: Request) {
  const body = await request.json();
  return NextResponse.json(body);
}
