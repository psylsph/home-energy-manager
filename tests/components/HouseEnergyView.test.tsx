import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, cleanup, screen, fireEvent } from '@testing-library/react';
import type { CurrentWeather, WeatherIconKind } from '../../src/lib/weatherIcon';
import type { CurrentCarbon } from '../../src/lib/carbonIntensity';

const weatherState: { value: CurrentWeather | null } = { value: null };
vi.mock('../../src/hooks/useLiveWeather', () => ({
  useLiveWeather: () => weatherState.value,
}));

const carbonState: { value: CurrentCarbon | null } = { value: null };
vi.mock('../../src/hooks/useCarbonIntensity', () => ({
  useCarbonIntensity: () => carbonState.value,
}));

import HouseEnergyView from '../../src/components/house/HouseEnergyView';
import { buildEnergyFlows, FLOW_COLORS } from '../../src/lib/energyFlow';
import { HOUSE, HOUSE_VIEWBOX, SKY_BAND, iso, skyPoint } from '../../src/lib/houseGeometry';
import { useInverterStore } from '../../src/store/useInverterStore';
import type { InverterSnapshot } from '../../src/lib/types';

/**
 * House view renderer (issue #422): draws the shared view model — same
 * numbers, status words and colours as the orbit — over the illustration.
 */

function snap(over: Partial<InverterSnapshot> = {}): InverterSnapshot {
  return {
    timestamp: 0,
    solar_power: 0, pv1_power: 0, pv2_power: 0,
    pv1_voltage: 0, pv2_voltage: 0, pv1_current: 0, pv2_current: 0,
    battery_power: 0, soc: 50, battery_voltage: 50, battery_current: 0,
    battery_state: 'idle', battery_temperature: 20, battery_capacity_kwh: 9.5,
    eps_power_w: 0, grid_power: 0, grid_voltage: 230, grid_frequency: 50,
    grid_online: true, grid_loss: false, inverter_trip: false,
    battery_over_temp: false, home_power: 0, inverter_temperature: 30,
    inverter_time: '',
    today_solar_kwh: 0, today_pv1_kwh: 0, today_pv2_kwh: 0,
    today_import_kwh: 0, today_export_kwh: 0, today_charge_kwh: 0,
    total_import_kwh: 0, total_export_kwh: 0, total_solar_kwh: 0,
    total_charge_kwh: 0, total_discharge_kwh: 0, total_throughput_kwh: 0,
    battery_mode: 'eco',
    ...over,
  } as InverterSnapshot;
}

const tid = (id: string) => document.querySelector(`[data-testid="${id}"]`);
const all = (id: string) => Array.from(document.querySelectorAll(`[data-testid="${id}"]`));

function setReducedMotion(reduce: boolean) {
  window.matchMedia = vi.fn().mockImplementation((q: string) => ({
    matches: reduce && q.includes('reduce'),
    media: q,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    onchange: null,
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

describe('<HouseEnergyView/>', () => {
  const originalMatchMedia = window.matchMedia;
  let savedNoise: number;

  beforeEach(() => {
    weatherState.value = null;
    carbonState.value = null;
    savedNoise = useInverterStore.getState().visualNoiseThreshold;
    useInverterStore.setState({ visualNoiseThreshold: 20 });
    setReducedMotion(false);
  });

  afterEach(() => {
    cleanup();
    window.matchMedia = originalMatchMedia;
    useInverterStore.setState({ visualNoiseThreshold: savedNoise });
  });

  it('draws one cable per part, in the orbit colours, with one cable off the panels', () => {
    const s = snap({ solar_power: 4000, pv1_power: 4000, battery_power: -1500, battery_state: 'charging', grid_power: 2000, home_power: 500 });
    render(<HouseEnergyView snapshot={s} connectionState="connected" gridFault={false} />);
    const vm = buildEnergyFlows(s, { noiseThresholdW: 20 });
    expect(vm.flows.length).toBe(3);
    const ids = [...document.querySelectorAll('[data-testid^="house-route-"]')].map((e) => e.getAttribute('data-testid'));
    expect(ids.sort()).toEqual(['house-route-battery', 'house-route-grid', 'house-route-home', 'house-route-solar']);
    for (const id of ids) expect(tid(id!)!.getAttribute('data-color')).toBe(FLOW_COLORS.solar);
  });

  it('marks which way each cable runs', () => {
    render(<HouseEnergyView snapshot={snap({ solar_power: 3000, pv1_power: 3000, home_power: 3000 })} connectionState="connected" gridFault={false} />);
    expect(tid('house-route-solar')!.getAttribute('data-supplies')).toBe('true');
    expect(tid('house-route-home')!.getAttribute('data-supplies')).toBe('false');
  });

  it('labels the diagram with the shared summary sentence', () => {
    const s = snap({ grid_power: -1500, home_power: 1500 });
    render(<HouseEnergyView snapshot={s} connectionState="connected" gridFault={false} />);
    const vm = buildEnergyFlows(s, { noiseThresholdW: 20 });
    expect(screen.getByRole('group', { name: /^Energy flow\./ }).getAttribute('aria-label')!.startsWith(`Energy flow. ${vm.summaryText}`)).toBe(true);
  });

  it('shows readings, status pills, SOC and the full battery mode', () => {
    const s = snap({
      battery_power: 3500, battery_state: 'discharging', home_power: 3500, soc: 33.5,
      battery_mode: 'timed_demand', enable_discharge: true,
      discharge_slots: [{ enabled: true, start_hour: 0, start_minute: 0, end_hour: 23, end_minute: 59, target_soc: 4 }],
    });
    render(<HouseEnergyView snapshot={s} connectionState="connected" gridFault={false} />);
    expect(tid('house-value-battery')!.textContent).toBe('3.5kW· 34%');
    expect(tid('house-status-battery')!.textContent).toBe('Discharging');
    expect(tid('house-status-grid')!.textContent).toBe('Idle');
    expect(tid('house-mode')!.textContent).toBe('Timed Demand (Discharging)');
    expect(tid('house-grid-state')!.textContent).toBe('On Grid');
    // Labels keep Title Case in the DOM (accessible text); CSS draws them upper-case.
    const label = Array.from(tid('house-reading-battery')!.querySelectorAll('text')).find((t) => t.textContent === 'Battery');
    expect(label).toBeDefined();
    expect((label as SVGTextElement).style.textTransform).toBe('uppercase');
    // Solar and Home have no pill.
    expect(tid('house-status-solar')).toBeNull();
    expect(tid('house-status-home')).toBeNull();
  });

  it('shows Grid Offline in the top pill on a grid fault', () => {
    render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault />);
    expect(tid('house-grid-state')!.textContent).toBe('Grid Offline');
    expect(tid('house-status-pill')!.getAttribute('data-tone')).toBe('fault');
  });

  it('shows Reconnecting while the connection is being restored', () => {
    render(<HouseEnergyView snapshot={snap()} connectionState="reconnecting" gridFault={false} />);
    expect(tid('house-grid-state')!.textContent).toBe('Reconnecting');
  });

  it('lights the panels only while generating', () => {
    const { rerender } = render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
    expect(tid('house-panel-glow')!.getAttribute('data-active')).toBe('false');
    rerender(<HouseEnergyView snapshot={snap({ solar_power: 3000, pv1_power: 3000, home_power: 3000 })} connectionState="connected" gridFault={false} />);
    expect(tid('house-panel-glow')!.getAttribute('data-active')).toBe('true');
  });

  it('fills the battery bars to the SOC', () => {
    render(<HouseEnergyView snapshot={snap({ soc: 62 })} connectionState="connected" gridFault={false} />);
    const bars = all('house-battery-bar');
    expect(bars).toHaveLength(10);
    expect(bars.filter((b) => b.getAttribute('data-lit') === 'true')).toHaveLength(6);
  });

  it('draws no flows for sub-threshold readings', () => {
    render(<HouseEnergyView snapshot={snap({ solar_power: 15, pv1_power: 15, battery_power: 15, grid_power: -15, home_power: 15 })} connectionState="connected" gridFault={false} />);
    expect(document.querySelectorAll('[data-testid^="house-route-"]')).toHaveLength(0);
    expect(tid('house-status-battery')!.textContent).toBe('Idle');
  });

  it('follows the noise threshold from Settings', () => {
    useInverterStore.setState({ visualNoiseThreshold: 100 });
    render(<HouseEnergyView snapshot={snap({ grid_power: -80, home_power: 80 })} connectionState="connected" gridFault={false} />);
    expect(tid('house-route-grid')).toBeNull();
  });

  it('animates the flows, unless reduced motion is requested', () => {
    const s = snap({ grid_power: -1500, home_power: 1500 });
    render(<HouseEnergyView snapshot={s} connectionState="connected" gridFault={false} />);
    expect(tid('house-route-grid')!.querySelector('animate')).not.toBeNull();
    cleanup();
    setReducedMotion(true);
    render(<HouseEnergyView snapshot={s} connectionState="connected" gridFault={false} />);
    expect(tid('house-route-grid')!.querySelector('animate')).toBeNull();
  });

  it('shows the EV post and reading only when a charger is configured', () => {
    const { rerender } = render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
    expect(tid('house-ev-post')).toBeNull();
    expect(tid('house-reading-ev')).toBeNull();
    rerender(
      <HouseEnergyView
        snapshot={snap({ home_power: 7500, grid_power: -7500 })}
        showEvc evcPower={7000} evcCharging evcConnected evcEverConnected evcChargingState="Charging"
        connectionState="connected" gridFault={false}
      />,
    );
    expect(tid('house-ev-post')).not.toBeNull();
    expect(tid('house-reading-ev')).not.toBeNull();
    expect(tid('house-route-ev')!.getAttribute('data-color')).toBe(FLOW_COLORS.home);
  });

  it('hides the weather when there is none to show', () => {
    render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
    expect(tid('house-weather')).toBeNull();
  });

  it.each<WeatherIconKind>([
    'clear-day', 'clear-night', 'partly-day', 'partly-night', 'cloudy', 'fog', 'drizzle', 'rain', 'snow', 'thunder',
  ])('draws the %s weather icon with the temperature', (kind) => {
    weatherState.value = { temperatureC: 10.2, kind, description: 'Test', solarWm2: null };
    render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
    const w = tid('house-weather')!;
    expect(w.getAttribute('data-kind')).toBe(kind);
    expect(w.getAttribute('aria-label')).toBe('Test, 10 °C');
    expect(w.textContent).toContain('10 °C');
    expect(w.querySelectorAll('path, circle, line').length).toBeGreaterThan(0);
    expect(tid('house-solar-intensity')).toBeNull();
  });

  it('shows the solar intensity under the temperature', () => {
    weatherState.value = { temperatureC: 14, kind: 'partly-day', description: 'Partly cloudy', solarWm2: 486 };
    render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
    expect(tid('house-weather')!.getAttribute('aria-label')).toBe('Partly cloudy, 14 °C, sunlight 486 W/m²');
    expect(tid('house-solar-value')!.textContent).toBe('486 W/m²');
    expect(tid('house-solar-intensity')!.getAttribute('data-level')).toBe('3');
    const bars = [...document.querySelectorAll('[data-testid="house-solar-bar"]')];
    expect(bars).toHaveLength(5);
    expect(bars.filter((b) => b.getAttribute('data-lit') === 'true')).toHaveLength(3);
    expect(bars[0].getAttribute('fill')).toBe(FLOW_COLORS.solar);
    expect(bars[4].getAttribute('fill')).toBe('var(--color-bg-elevated)');
  });

  it('shows a daytime zero with no bars lit', () => {
    weatherState.value = { temperatureC: 3, kind: 'fog', description: 'Fog', solarWm2: 0 };
    render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
    expect(tid('house-solar-value')!.textContent).toBe('0 W/m²');
    expect(tid('house-solar-intensity')!.getAttribute('data-level')).toBe('0');
  });

  describe('grid carbon intensity', () => {
    const lowAt = new Date(2026, 10, 2, 3, 30).getTime();
    const carbon: CurrentCarbon = { intensity: 183, band: 'high', region: 'South England', lowest: { at: lowAt, intensity: 92 } };
    const view = () => render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
    const y = () => tid('house-carbon-value')!.getAttribute('y');

    it('hides when there is none to show', () => {
      view();
      expect(tid('house-carbon')).toBeNull();
    });

    it('shows the intensity in the band colour and the lowest time ahead', () => {
      carbonState.value = carbon;
      view();
      const c = tid('house-carbon')!;
      expect(c.getAttribute('data-band')).toBe('high');
      expect(c.getAttribute('aria-label')).toBe('Grid carbon 183 g/kWh, high, South England. Lowest 03:30 at 92 g/kWh.');
      expect(tid('house-carbon-leaf')!.getAttribute('fill')).toBe('#F97316');
      // Just the figure: when it's lowest is in the label, not drawn.
      expect(tid('house-carbon-value')!.textContent).toBe('183 g/kWh');
    });

    it('leaves a clear gap above the EV reading', () => {
      carbonState.value = carbon;
      weatherState.value = { temperatureC: 14, kind: 'partly-day', description: 'Partly cloudy', solarWm2: 486 };
      render(
        <HouseEnergyView
          snapshot={snap({ home_power: 7500, grid_power: -7500 })}
          showEvc evcPower={7000} evcCharging evcConnected evcEverConnected evcChargingState="Charging"
          connectionState="connected" gridFault={false}
        />,
      );
      const carbonBaseline = Number(y());
      const evBaseline = Number(tid('house-value-ev')!.getAttribute('y'));
      // The EV digits are ~16 px tall (22 px font): their top must sit well
      // below the carbon line.
      expect(evBaseline - 16 - carbonBaseline).toBeGreaterThanOrEqual(20);
    });

    it('keeps the widest EV status pill left of the roof eave', () => {
      render(
        <HouseEnergyView
          snapshot={snap()}
          showEvc evcEverConnected
          connectionState="connected" gridFault={false}
        />,
      );
      const pill = tid('house-status-ev')!;
      expect(pill.textContent).toBe('Disconnected');
      const rect = pill.querySelector('rect')!;
      const right = Number(rect.getAttribute('x')) + Number(rect.getAttribute('width'));
      const bottom = Number(rect.getAttribute('y')) + Number(rect.getAttribute('height'));
      // The eave runs from the front-left corner up towards the ridge.
      const eave = iso(HOUSE.x0, HOUSE.y1, HOUSE.wall);
      const ridge = iso(HOUSE.x0, (HOUSE.y0 + HOUSE.y1) / 2, HOUSE.ridge);
      const roofX = eave.x + ((eave.y - bottom) / (eave.y - ridge.y)) * (ridge.x - eave.x);
      expect(right).toBeLessThan(roofX - 3);
    });

    it('greys an unknown band', () => {
      carbonState.value = { ...carbon, band: null, lowest: null };
      view();
      expect(tid('house-carbon-leaf')!.getAttribute('fill')).toBe('var(--app-text-secondary, #8B949E)');
    });

    it('sits under the solar line, or moves up when the weather lines are missing', () => {
      carbonState.value = carbon;
      weatherState.value = { temperatureC: 14, kind: 'partly-day', description: 'Partly cloudy', solarWm2: 486 };
      view();
      expect(y()).toBe('104');
      cleanup();
      weatherState.value = { temperatureC: 14, kind: 'clear-night', description: 'Clear', solarWm2: null };
      view();
      expect(y()).toBe('86');
      cleanup();
      weatherState.value = null;
      view();
      expect(y()).toBe('68');
    });
  });

  describe('sky band', () => {
    const ALTON = { latitude: 51.1058, longitude: -1.048 };
    const withSky = (iso: string, location: CurrentWeather['location'] | null = ALTON) => {
      vi.useFakeTimers({ toFake: ['Date'] });
      vi.setSystemTime(Date.parse(iso));
      weatherState.value = { temperatureC: 9, kind: 'cloudy', description: 'Cloudy', solarWm2: null, location: location ?? undefined };
      return render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
    };
    const viewBoxHeight = () => Number(document.querySelector('svg')!.getAttribute('viewBox')!.split(' ')[3]);
    const at = (el: Element) => el.getAttribute('transform')!.match(/translate\(([\d.]+) ([\d.]+)\)/)!.slice(1).map(Number);

    afterEach(() => vi.useRealTimers());

    it('draws no sky, and leaves the house where it was, without a location', () => {
      withSky('2026-11-01T12:00:00Z', null);
      expect(tid('house-sky')).toBeNull();
      expect(viewBoxHeight()).toBe(HOUSE_VIEWBOX.height);
      expect(tid('house-scene')!.getAttribute('transform')).toBeNull();
    });

    it('moves the house down to make room for the sky band', () => {
      withSky('2026-11-01T12:00:00Z');
      expect(viewBoxHeight()).toBe(HOUSE_VIEWBOX.height + SKY_BAND);
      expect(tid('house-scene')!.getAttribute('transform')).toBe(`translate(0 ${SKY_BAND})`);
      // Weather, readings and the house all move; the status pill doesn't.
      expect(tid('house-scene')!.contains(tid('house-weather'))).toBe(true);
      expect(tid('house-scene')!.contains(tid('house-reading-solar'))).toBe(true);
      expect(tid('house-scene')!.contains(tid('house-status-pill'))).toBe(false);
    });

    it('puts the sun near the top of its arc at midday, with the day\'s times at the ends', () => {
      withSky('2026-11-01T11:45:00Z');
      const sky = tid('house-sky')!;
      expect(sky.getAttribute('data-body')).toBe('sun');
      const [x, y] = at(tid('house-sun')!);
      const top = skyPoint(0.5);
      expect(Math.abs(x - top.x)).toBeLessThan(5);
      expect(Math.abs(y - top.y)).toBeLessThan(1);
      const local = (iso: string) => {
        const d = new Date(iso);
        return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
      };
      // Alton, 1 Nov: sunrise ~06:57, sunset ~16:39 UTC.
      expect([local('2026-11-01T06:56:00Z'), local('2026-11-01T06:57:00Z'), local('2026-11-01T06:58:00Z')]).toContain(tid('house-sunrise')!.textContent);
      expect([local('2026-11-01T16:38:00Z'), local('2026-11-01T16:39:00Z'), local('2026-11-01T16:40:00Z')]).toContain(tid('house-sunset')!.textContent);
      expect(sky.getAttribute('aria-label')).toMatch(/^Sunrise \d\d:\d\d, sunset \d\d:\d\d$/);
    });

    it('shows the moon at night, early on its path in the evening', () => {
      withSky('2026-11-01T18:00:00Z');
      expect(tid('house-sun')).toBeNull();
      const [x] = at(tid('house-moon')!);
      expect(x).toBeLessThan(skyPoint(0.25).x);
    });

    it('keeps the sun clear of the status pill at the top of its arc', () => {
      withSky('2026-11-01T11:45:00Z');
      const [, y] = at(tid('house-sun')!);
      const pill = tid('house-status-pill')!.querySelector('rect')!;
      const pillBottom = Number(pill.getAttribute('y')) + Number(pill.getAttribute('height'));
      // Rays reach 10.5 from the centre.
      expect(y - 10.5).toBeGreaterThan(pillBottom);
    });

    it('draws no sky where the sun never sets', () => {
      withSky('2026-06-21T12:00:00Z', { latitude: 78.2, longitude: 15.6 });
      expect(tid('house-sky')).toBeNull();
      expect(viewBoxHeight()).toBe(HOUSE_VIEWBOX.height);
    });
  });

  it("shows Solar's share of the array on the label line", () => {
    const arrays = [{ source: 'pv1' as const, name: '', power_w: 5200, rated_kw: 8.4 }];
    render(<HouseEnergyView snapshot={snap({ solar_power: 5200, pv1_power: 5200, home_power: 5200, solar_arrays: arrays })} connectionState="connected" gridFault={false} />);
    expect(tid('house-percent-solar')!.textContent).toBe('· 62%');
    expect(tid('house-value-solar')!.textContent).toBe('5.2kW');
    expect(tid('house-percent-home')).toBeNull();
  });

  describe('volts and amps tooltip', () => {
    const s = () => snap({
      solar_power: 3000, pv1_power: 3000, pv1_voltage: 350, pv1_current: 8.5,
      battery_power: -2000, battery_state: 'charging', battery_voltage: 52.1, battery_current: -38.4,
      grid_power: 500, grid_voltage: 241.3, grid_frequency: 50, home_power: 500,
    });
    const tip = () => document.querySelector('[data-testid="house-detail-tip"]');
    const mouse = (el: Element, type: 'pointerEnter' | 'pointerLeave') =>
      fireEvent[type](el, { pointerType: 'mouse' });

    it('shows on hover and hides when the mouse leaves', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      expect(tip()).toBeNull();
      mouse(tid('house-reading-battery')!, 'pointerEnter');
      expect(tip()!.getAttribute('data-for')).toBe('battery');
      expect(tip()!.textContent).toMatch(/V · .*A$/);
      mouse(tid('house-reading-battery')!, 'pointerLeave');
      expect(tip()).toBeNull();
    });

    it('keeps the bubble open when a mouse clicks the reading it is hovering', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      const battery = tid('house-reading-battery')!;
      mouse(battery, 'pointerEnter');
      // The pointer type comes from pointerdown: older engines send click as a plain MouseEvent.
      fireEvent.pointerDown(battery, { pointerType: 'mouse' });
      fireEvent.click(battery);
      expect(tip()!.getAttribute('data-for')).toBe('battery');
      fireEvent.pointerDown(battery, { pointerType: 'mouse' });
      fireEvent.click(battery);
      expect(tip()!.getAttribute('data-for')).toBe('battery');
    });

    it('ignores touch hover, so a tap is what opens it', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      fireEvent.pointerEnter(tid('house-reading-solar')!, { pointerType: 'touch' });
      expect(tip()).toBeNull();
      fireEvent.click(tid('house-hit-solar')!);
      expect(tip()!.getAttribute('data-for')).toBe('solar');
      fireEvent.pointerLeave(tid('house-reading-solar')!, { pointerType: 'touch' });
      expect(tip()).not.toBeNull();
    });

    it('toggles on tap, moves to another reading, and closes on a tap elsewhere', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      fireEvent.click(tid('house-reading-grid')!);
      expect(tip()!.getAttribute('data-for')).toBe('grid');
      expect(tip()!.textContent).toMatch(/Hz$/);
      fireEvent.click(tid('house-reading-solar')!);
      expect(tip()!.getAttribute('data-for')).toBe('solar');
      fireEvent.click(tid('house-reading-solar')!);
      expect(tip()).toBeNull();
      fireEvent.click(tid('house-reading-solar')!);
      fireEvent.click(screen.getByRole('group', { name: /^Energy flow\./ }));
      expect(tip()).toBeNull();
    });

    it('leaves a moved-off hover alone when another reading owns the tip', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      fireEvent.click(tid('house-reading-grid')!);
      mouse(tid('house-reading-solar')!, 'pointerLeave');
      expect(tip()!.getAttribute('data-for')).toBe('grid');
    });

    it('places the bubble under a top-row reading and above a bottom-row one, inside the view', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      const box = () => {
        const rect = tip()!.querySelector('rect')!;
        return { x: Number(rect.getAttribute('x')), y: Number(rect.getAttribute('y')), w: Number(rect.getAttribute('width')) };
      };
      fireEvent.click(tid('house-reading-solar')!);
      expect(box().y).toBeGreaterThan(70);
      fireEvent.click(tid('house-reading-battery')!);
      expect(box().y).toBeLessThan(446 - 22);
      expect(box().x + box().w).toBeLessThanOrEqual(390);
    });

    it('toggles on a touch tap that says so on pointerdown', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      const solar = tid('house-reading-solar')!;
      fireEvent.pointerDown(solar, { pointerType: 'touch' });
      fireEvent.click(solar);
      expect(tip()!.getAttribute('data-for')).toBe('solar');
      fireEvent.pointerDown(solar, { pointerType: 'touch' });
      fireEvent.click(solar);
      expect(tip()).toBeNull();
    });

    it('closes on a tap outside the diagram, but not on one inside it', () => {
      render(
        <div>
          <HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />
          <button type="button">Elsewhere</button>
        </div>,
      );
      fireEvent.click(tid('house-reading-grid')!);
      fireEvent.pointerDown(tid('house-reading-grid')!);
      expect(tip()).not.toBeNull();
      fireEvent.pointerDown(screen.getByRole('button', { name: 'Elsewhere' }));
      expect(tip()).toBeNull();
    });

    it('gives screen readers the volts and amps in the diagram label', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      const label = screen.getByRole('group', { name: /^Energy flow\./ }).getAttribute('aria-label')!;
      expect(label).toMatch(/Solar \S+V · \S+A\./);
      expect(label).toMatch(/Grid \S+V · \S+Hz\./);
      expect(label).toMatch(/Battery \S+V · \S+A\./);
    });

    it('forgets an open bubble when its reading loses the detail, so it never reopens by itself', () => {
      const { rerender } = render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      fireEvent.click(tid('house-reading-battery')!);
      expect(tip()).not.toBeNull();
      // One poll without a battery voltage: no detail, no bubble…
      rerender(<HouseEnergyView snapshot={{ ...s(), battery_voltage: 0 }} connectionState="connected" gridFault={false} />);
      expect(tip()).toBeNull();
      // …and when the voltage comes back the bubble stays shut.
      rerender(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      expect(tip()).toBeNull();
    });

    it('opens from the keyboard: each reading with volts / amps is a focusable button', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      const solar = screen.getByRole('button', { name: 'Solar volts and amps' });
      expect(solar.getAttribute('tabindex')).toBe('0');
      expect(solar.getAttribute('aria-expanded')).toBe('false');
      fireEvent.keyDown(solar, { key: 'Enter' });
      expect(tip()!.getAttribute('data-for')).toBe('solar');
      expect(solar.getAttribute('aria-expanded')).toBe('true');
      fireEvent.keyDown(solar, { key: ' ' });
      expect(tip()).toBeNull();
      fireEvent.keyDown(solar, { key: ' ' });
      fireEvent.keyDown(solar, { key: 'Escape' });
      expect(tip()).toBeNull();
      fireEvent.keyDown(solar, { key: 'Enter' });
      fireEvent.keyDown(solar, { key: 'a' });
      expect(tip()).not.toBeNull();
      fireEvent.blur(solar);
      expect(tip()).toBeNull();
      expect(screen.queryByRole('button', { name: 'Home volts and amps' })).toBeNull();
    });

    it('gives Home no tooltip', () => {
      render(<HouseEnergyView snapshot={s()} connectionState="connected" gridFault={false} />);
      expect(tid('house-hit-home')).toBeNull();
      fireEvent.click(tid('house-reading-home')!);
      expect(tip()).toBeNull();
      mouse(tid('house-reading-home')!, 'pointerEnter');
      expect(tip()).toBeNull();
    });
  });

  describe('themes', () => {
    afterEach(() => {
      delete document.documentElement.dataset.theme;
    });

    it.each(['dark', 'light'])('paints the house only from theme variables in %s mode', (theme) => {
      document.documentElement.dataset.theme = theme;
      render(<HouseEnergyView snapshot={snap()} connectionState="connected" gridFault={false} />);
      const illo = tid('house-illustration')!;
      const fills = Array.from(illo.querySelectorAll('[fill], [stroke]')).flatMap((el) =>
        ['fill', 'stroke'].map((a) => el.getAttribute(a)).filter((v): v is string => !!v && v !== 'none'),
      );
      expect(fills.length).toBeGreaterThan(50);
      for (const value of fills) {
        // Theme colours, or the inverter's flow colour (as on the orbit).
        expect(value === FLOW_COLORS.inverter || value.startsWith('var(--app-house-'), value).toBe(true);
      }
      // Readings take theme text colours too, so light mode needs no artwork of its own.
      expect(tid('house-value-grid')!.getAttribute('fill')).toBe('var(--app-text-primary, #F0F6FC)');
    });
  });
});
