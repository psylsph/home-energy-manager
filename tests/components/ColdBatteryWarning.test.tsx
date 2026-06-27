import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';

vi.mock('../../src/lib/api', () => ({
  apiGet: vi.fn(async (path: string) => {
    if (path === '/api/auto-winter') {
      return { ok: true, data: { config: { enabled: false } } };
    }
    if (path === '/api/alerts') {
      return { ok: true, data: { config: { batt_temp_min: 5 } } };
    }
    return { ok: true, data: {} };
  }),
}));

// Imports after the vi.mock (which vitest hoists to the top) so the mock
// applies to the module the component resolves.
import { apiGet } from '../../src/lib/api';
import ColdBatteryWarning from '../../src/components/ColdBatteryWarning';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

// The component renders a <Link> (react-router), so it needs a router context.
function renderWarning() {
  return render(
    <MemoryRouter>
      <ColdBatteryWarning />
    </MemoryRouter>,
  );
}

function setSnapshot(batteryTemperature: number | null): void {
  useInverterStore.setState({
    snapshot: {
      battery_temperature: batteryTemperature as number,
    } as InverterSnapshot,
  });
}

describe('<ColdBatteryWarning/>', () => {
  beforeEach(() => {
    cleanup();
    useInverterStore.setState({
      snapshot: null,
      developerMode: false,
    });
    vi.mocked(apiGet).mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it('renders nothing when there is no snapshot yet', () => {
    const { container } = renderWarning();
    expect(container.querySelector('.bg-blue-900\\/30')).toBeNull();
  });

  it('renders the warning when the battery is cold and the alert threshold is set', async () => {
    setSnapshot(2.0);
    const { container } = renderWarning();
    await waitFor(() => {
      expect(container.querySelector('.bg-blue-900\\/30')).not.toBeNull();
    });
    expect(container.textContent).toContain('Cold battery');
    expect(container.textContent).toContain('2.0°C');
  });

  it('does not warn when the battery temperature is above the threshold', async () => {
    setSnapshot(20.0);
    const { container } = renderWarning();
    // Give the async config fetches a tick to settle, then confirm no warning.
    await waitFor(() => {
      expect(vi.mocked(apiGet)).toHaveBeenCalledWith('/api/alerts');
    });
    expect(container.querySelector('.bg-blue-900\\/30')).toBeNull();
  });

  it('renders nothing and does not throw when battery_temperature is null (Gateway)', async () => {
    // The Gateway (DTC 0x70xx) doesn't expose per-pack temperature — the
    // backend sets f32::NAN, serde_json serialises it as null. Before the
    // fix the component called .toFixed(1) on null and crashed
    // ("Cannot read properties of null (reading 'toFixed')").
    setSnapshot(null);
    const { container } = renderWarning();
    // Let the async config fetches settle so we're not racing the effect.
    await waitFor(() => {
      expect(vi.mocked(apiGet)).toHaveBeenCalledWith('/api/alerts');
    });
    expect(container.querySelector('.bg-blue-900\\/30')).toBeNull();
  });
});
