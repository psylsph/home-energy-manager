import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import PlannerModeWarning from '../../src/components/PlannerModeWarning';
import type { InverterSnapshot } from '../../src/lib/types';

afterEach(cleanup);

const snap = (charging_mode: InverterSnapshot['charging_mode']) =>
  ({ charging_mode }) as InverterSnapshot;

describe('PlannerModeWarning', () => {
  it('links through to the Control page inside the app', () => {
    render(
      <MemoryRouter>
        <PlannerModeWarning snapshot={snap('cosy')} />
      </MemoryRouter>,
    );
    expect(screen.getByRole('link', { name: 'Control page' }).getAttribute('href')).toBe('/control');
  });

  it('renders nothing in Standard mode', () => {
    const { container } = render(
      <MemoryRouter>
        <PlannerModeWarning snapshot={snap('standard')} />
      </MemoryRouter>,
    );
    expect(container.innerHTML).toBe('');
  });

  it('renders nothing without a snapshot', () => {
    const { container } = render(
      <MemoryRouter>
        <PlannerModeWarning snapshot={null} />
      </MemoryRouter>,
    );
    expect(container.innerHTML).toBe('');
  });
});
