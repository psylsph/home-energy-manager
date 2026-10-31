import { fireEvent, screen } from '@testing-library/react';

/**
 * Unfold every Settings "Advanced" disclosure on screen, as a user would
 * before reaching the rarely changed fields inside.
 */
export async function openAdvanced() {
  for (const button of await screen.findAllByRole('button', { name: 'Advanced' })) {
    if (button.getAttribute('aria-expanded') !== 'true') fireEvent.click(button);
  }
}
