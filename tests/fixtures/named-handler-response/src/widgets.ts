export interface Widget {
  id: string;
  name: string;
  parts: number;
}

export interface NewWidget {
  name: string;
  parts: number;
}

export async function listAll(): Promise<Widget[]> {
  return [];
}

export async function store(input: NewWidget): Promise<Widget> {
  return { id: 'w1', ...input };
}
