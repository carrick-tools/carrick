import { useQuery } from '@example/gql-client';
import { NoteDocument } from '../generated/graphql';

// Note — lecture seule
export function NotePage({ id }: { id: string }) {
  const [note] = useQuery(NoteDocument, { id });

  async function health(): Promise<unknown> {
    const res = await fetch(`${process.env.BILLING_API_URL}/health`);
    return res.json();
  }

  return { note, health };
}
