import { LinkButton } from '../components/Buttons';

export function NotFound() {
  return (
    <section className="page-narrow">
      <p className="label">404</p>
      <h1 className="page-title">Nothing here.</h1>
      <p className="lede">That page does not exist.</p>
      <div className="cta-row">
        <LinkButton to="/" variant="primary">
          Back to the overview
        </LinkButton>
      </div>
    </section>
  );
}
