import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import CosyBlockedNotice from '../../src/components/CosyBlockedNotice';

afterEach(cleanup);

describe('CosyBlockedNotice', () => {
  it('shows an accessible warning while Cosy is held back', () => {
    render(<CosyBlockedNotice blockedBy="pause" enabled />);
    const notice = screen.getByRole('status');
    expect(notice).toHaveTextContent(/Cosy is on hold/);
    expect(notice).toHaveTextContent(/battery pause/i);
  });

  it('renders nothing when Cosy is not blocked', () => {
    const { container } = render(<CosyBlockedNotice blockedBy="" enabled />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders nothing when Cosy is off', () => {
    const { container } = render(<CosyBlockedNotice blockedBy="safety" enabled={false} />);
    expect(container).toBeEmptyDOMElement();
  });
});
