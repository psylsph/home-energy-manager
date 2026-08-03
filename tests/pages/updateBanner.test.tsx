import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';

// `openExternal` opens a URL in the system browser — stub it so we can
// assert the banner hands it the release URL without leaving jsdom.
vi.mock('../../src/lib/openExternal', () => ({
  openExternal: vi.fn(async () => {}),
}));

import UpdateBanner from '../../src/components/UpdateBanner';
import { useInverterStore } from '../../src/store/useInverterStore';
import { openExternal } from '../../src/lib/openExternal';

const baseInfo = {
  current_version: '0.70.2',
  latest_version: '0.70.3',
  release_url: 'https://github.com/psylsph/home-energy-manager/releases/tag/v0.70.3',
  update_available: true,
  checking: false,
  last_checked_at: null,
  last_error: null,
} as const;

describe('UpdateBanner', () => {
  beforeEach(() => {
    useInverterStore.setState({
      latestVersionInfo: null,
      dismissedUpdateVersion: null,
    });
    vi.mocked(openExternal).mockClear();
  });
  afterEach(() => cleanup());

  it('shows when a newer version is available and not dismissed', () => {
    useInverterStore.setState({ latestVersionInfo: { ...baseInfo } });
    render(<UpdateBanner />);
    expect(screen.getByText(/a new version is available/i)).toBeTruthy();
    expect(screen.getByText(/v0\.70\.3/i)).toBeTruthy();
    // The "you're running" hint shows the current version too.
    expect(screen.getByText(/you.re running v0\.70\.2/i)).toBeTruthy();
  });

  it('is hidden when the user has dismissed that exact release', () => {
    useInverterStore.setState({
      latestVersionInfo: { ...baseInfo },
      dismissedUpdateVersion: '0.70.3',
    });
    const { container } = render(<UpdateBanner />);
    expect(container.children).toHaveLength(0);
  });

  it('reappears for a newer release after an older one was dismissed', () => {
    useInverterStore.setState({
      latestVersionInfo: { ...baseInfo, latest_version: '0.70.4' },
      dismissedUpdateVersion: '0.70.3', // dismissed the previous release
    });
    render(<UpdateBanner />);
    expect(screen.getByText(/v0\.70\.4/i)).toBeTruthy();
  });

  it('is hidden when no latest version is known (cold cache)', () => {
    useInverterStore.setState({
      latestVersionInfo: { ...baseInfo, latest_version: null, update_available: false },
    });
    const { container } = render(<UpdateBanner />);
    expect(container.children).toHaveLength(0);
  });

  it('is hidden when the latest is not actually newer', () => {
    useInverterStore.setState({
      latestVersionInfo: { ...baseInfo, latest_version: '0.70.2' },
    });
    const { container } = render(<UpdateBanner />);
    expect(container.children).toHaveLength(0);
  });

  it('is hidden when update checking is disabled', () => {
    useInverterStore.setState({
      latestVersionInfo: { ...baseInfo, disabled: true },
    });
    const { container } = render(<UpdateBanner />);
    expect(container.children).toHaveLength(0);
  });

  it('opens the release URL when "View release" is clicked', () => {
    useInverterStore.setState({ latestVersionInfo: { ...baseInfo } });
    render(<UpdateBanner />);
    fireEvent.click(screen.getByRole('button', { name: /view release/i }));
    expect(openExternal).toHaveBeenCalledWith(baseInfo.release_url);
  });

  it('records a per-version dismissal when the ✕ is clicked', () => {
    useInverterStore.setState({ latestVersionInfo: { ...baseInfo } });
    render(<UpdateBanner />);
    fireEvent.click(screen.getByRole('button', { name: /dismiss update notice/i }));
    expect(useInverterStore.getState().dismissedUpdateVersion).toBe('0.70.3');
  });
});
