import { useQuery } from '@example/gql-client';
import { LedgerDocument } from '../generated/graphql';

// Grand livre — écritures
export function LedgerPage() {
  const [ledger] = useQuery(LedgerDocument);
  return ledger;
}
