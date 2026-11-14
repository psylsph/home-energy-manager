import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import ReadOnlyHidden from '../../src/components/ReadOnlyHidden';
import { useInverterStore } from '../../src/store/useInverterStore';

describe('<ReadOnlyHidden/> (issue #471)', () => {
  afterEach(() => {
    cleanup();
    useInverterStore.setState({ readOnly: false });
    window.history.replaceState({}, '', '/');
  });

  it('hides on the first render of a ?RO address, before App sets the store', () => {
    // App sets the store flag in an effect; until then the address decides,
    // as it does for App's own nav.
    for (const url of ['/?RO', '/#/forecast?RO']) {
      cleanup();
      window.history.replaceState({}, '', url);
      useInverterStore.setState({ readOnly: false });
      const { container } = render(<ReadOnlyHidden><button type="button">Apply</button></ReadOnlyHidden>);
      expect(container.innerHTML, url).toBe('');
    }
  });

  it('shows its children normally', () => {
    useInverterStore.setState({ readOnly: false });
    render(<ReadOnlyHidden><button type="button">Apply</button></ReadOnlyHidden>);
    expect(screen.getByRole('button', { name: 'Apply' })).toBeTruthy();
  });

  it('renders nothing on a read-only link', () => {
    useInverterStore.setState({ readOnly: true });
    const { container } = render(
      <ReadOnlyHidden><button type="button">Apply</button></ReadOnlyHidden>,
    );
    expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
    expect(container.innerHTML).toBe('');
  });
});
