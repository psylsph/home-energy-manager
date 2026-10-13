import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import AgileBlockedNotice from '../../src/components/AgileBlockedNotice';

afterEach(cleanup);

describe('AgileBlockedNotice', () => {
  it('shows an accessible warning while Agile is held back', () => {
    render(<AgileBlockedNotice blockedBy="timed_export" enabled />);
    const notice = screen.getByRole('status');
    expect(notice).toHaveTextContent(/Agile is on hold/);
    expect(notice).toHaveTextContent(/Timed Export schedule/);
  });

  it('renders nothing when Agile is not blocked', () => {
    const { container } = render(<AgileBlockedNotice blockedBy="" enabled />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders nothing when Agile is off', () => {
    const { container } = render(<AgileBlockedNotice blockedBy="safety" enabled={false} />);
    expect(container).toBeEmptyDOMElement();
  });
});
