import { useQuery } from '@example/gql-client';
import { ConsignmentDocument } from '../generated/graphql';

export function ConsignmentPage({ id }: { id: string }) {
  const [consignment] = useQuery(ConsignmentDocument, { id });
  return consignment;
}
