import { NextResponse } from 'next/server';

export async function GET(request: Request) {
  const { searchParams } = new URL(request.url);
  return NextResponse.json({ from: searchParams.get('from'), to: searchParams.get('to'), rows: [] });
}
