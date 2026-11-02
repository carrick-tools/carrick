import { useQuery } from '@example/gql-client';
import { ParcelDocument } from '../generated/graphql';

export function ParcelPage({ id }: { id: string }) {
  const [parcel] = useQuery(ParcelDocument, { id });
  return parcel;
}
