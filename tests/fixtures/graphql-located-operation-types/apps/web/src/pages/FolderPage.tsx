import { useQuery } from '@example/gql-client';
import { FolderDocument } from '../generated/graphql';

// Dossier — contenu et dossier parent
export function FolderPage({ id }: { id: string }) {
  const [folder] = useQuery(FolderDocument, { id });

  async function health(): Promise<unknown> {
    const res = await fetch(`${process.env.BILLING_API_URL}/health`);
    return res.json();
  }

  return { folder, health };
}
