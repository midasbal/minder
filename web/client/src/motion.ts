import type { Variants } from 'framer-motion';

export const EASE = [0.22, 1, 0.36, 1] as const;

export const stagger: Variants = {
  hidden: {},
  shown: { transition: { staggerChildren: 0.12, delayChildren: 0.15 } },
};

export const rise: Variants = {
  hidden: { opacity: 0, y: 20 },
  shown: { opacity: 1, y: 0, transition: { duration: 0.8, ease: EASE } },
};

/** The same entrance, after the headline has landed. */
export const riseAfterHeadline: Variants = {
  hidden: { opacity: 0, y: 20 },
  shown: { opacity: 1, y: 0, transition: { duration: 0.8, ease: EASE, delay: 0.75 } },
};

// Grows from the origin set in CSS: the top of the vertical rail, the left of the horizontal one.
export const draw: Variants = {
  hidden: { scale: 0 },
  shown: { scale: 1, transition: { duration: 1.2, ease: EASE, delay: 0.3 } },
};

/** For blocks below the fold: reveal once, as they scroll into view. */
export const reveal = {
  variants: rise,
  initial: 'hidden',
  whileInView: 'shown',
  viewport: { once: true, amount: 0.15 },
} as const;
