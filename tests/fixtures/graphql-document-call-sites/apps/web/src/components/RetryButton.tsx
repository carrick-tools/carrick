import { useMutation } from '@example/gql-client';
import { PlaceOrderDocument } from '@/generated/documents';

export function RetryButton() {
  const [retry] = useMutation(PlaceOrderDocument);
  return retry;
}
