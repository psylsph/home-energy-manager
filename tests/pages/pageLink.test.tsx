import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import PageLink from '../../src/components/PageLink';

afterEach(cleanup);

describe('PageLink', () => {
  it('is a link to the page inside a router', () => {
    render(<MemoryRouter><PageLink to="/settings">Settings</PageLink></MemoryRouter>);
    expect(screen.getByRole('link', { name: 'Settings' }).getAttribute('href')).toBe('/settings');
  });

  it('falls back to its plain text outside a router', () => {
    render(<PageLink to="/settings">Settings</PageLink>);
    expect(screen.queryByRole('link')).toBeNull();
    expect(screen.getByText('Settings')).toBeTruthy();
  });
});
