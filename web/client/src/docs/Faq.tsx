import { FAQ, FaqList } from '../faq';
import { DocSection } from './parts';

export function Faq() {
  return (
    <DocSection id="faq" number="6" title="Frequently asked questions">
      <p>
        Short, sourced answers to the questions people ask first. Each says what holds and what does not; the sections above
        have the detail.
      </p>
      <FaqList items={FAQ} />
    </DocSection>
  );
}
