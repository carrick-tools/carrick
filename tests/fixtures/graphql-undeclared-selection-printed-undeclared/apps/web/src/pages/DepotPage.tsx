import { useQuery } from '@example/gql-client';
import { DepotDocument } from '../generated/graphql';

export function DepotPage({ code }: { code: string }) {
  const [depot] = useQuery(DepotDocument, { code });
  return depot;
}
