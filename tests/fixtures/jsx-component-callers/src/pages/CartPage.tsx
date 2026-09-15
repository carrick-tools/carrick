import { PriceTag } from '../components/PriceTag';

// An ARROW component with an expression body, rendering the element with
// children around it.
export const CartPage = ({ subtotal, shipping }: { subtotal: number; shipping: number }) => (
  <section>
    <p>
      Subtotal <PriceTag amount={subtotal} currency="EUR" />
    </p>
    <p>
      Total <PriceTag amount={subtotal + shipping} currency="EUR" />
    </p>
  </section>
);
