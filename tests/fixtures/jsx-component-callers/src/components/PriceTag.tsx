export interface PriceTagProps {
  amount: number;
  currency: string;
}

// The component every page renders. Its callers are the answer key.
export function PriceTag({ amount, currency }: PriceTagProps) {
  return (
    <span className="price">
      {amount.toFixed(2)} {currency}
    </span>
  );
}
