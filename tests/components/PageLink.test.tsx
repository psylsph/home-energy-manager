import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import PageLink from '../../src/components/PageLink';
import { useInverterStore } from '../../src/store/useInverterStore';

describe('<PageLink/>', () => {
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ readOnly: false });
    window.history.replaceState({}, '', '/');
  });

  it('is plain text on the first render of a ?RO address too', () => {
    window.history.replaceState({}, '', '/#/forecast?RO');
    inRouter('/settings');
    expect(screen.queryByRole('link')).toBeNull();
  });

  const inRouter = (to: string) =>
    render(<MemoryRouter><p>Turn it on from the <PageLink to={to}>Control page</PageLink>.</p></MemoryRouter>);

  it('links to the page', () => {
    inRouter('/control');
    expect(screen.getByRole('link', { name: 'Control page' }).getAttribute('href')).toBe('/control');
  });

  it('is plain text outside a router', () => {
    render(<PageLink to="/control">Control page</PageLink>);
    expect(screen.queryByRole('link')).toBeNull();
    expect(screen.getByText('Control page')).toBeTruthy();
  });

  it("is plain text on a read-only link for a page it can't reach, so the sentence still reads (issue #471)", () => {
    useInverterStore.setState({ readOnly: true });
    const { container } = inRouter('/control');
    expect(screen.queryByRole('link')).toBeNull();
    expect(container.textContent).toBe('Turn it on from the Control page.');
    cleanup();
    inRouter('/settings');
    expect(screen.queryByRole('link')).toBeNull();
  });

  it('still links read-only to a page the link can reach', () => {
    useInverterStore.setState({ readOnly: true });
    inRouter('/forecast');
    expect(screen.getByRole('link', { name: 'Control page' })).toBeTruthy();
  });
});
