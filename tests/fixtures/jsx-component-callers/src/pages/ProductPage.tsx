import { PriceTag } from '../components/PriceTag';
import * as Icons from '../components/icons';

// A local function that shares its name with an intrinsic element. The
// `<nav>` below is the host element, not a call to this function.
export function nav(sections: string[]) {
  return sections.join(' / ');
}

// RELATIVE import, self-closing element, plus a member element through a
// namespace import.
export function ProductPage({ title, amount }: { title: string; amount: number }) {
  return (
    <article>
      <nav>{title}</nav>
      <Icons.Star />
      <PriceTag amount={amount} currency="EUR" />
    </article>
  );
}
