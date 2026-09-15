import { PriceTag } from '@/components/PriceTag';

// ALIAS import through tsconfig `paths`.
export default function CheckoutSummary({ total }: { total: number }) {
  return (
    <footer>
      <PriceTag amount={total} currency="EUR" />
    </footer>
  );
}
