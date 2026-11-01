import { useCallback, useEffect, useRef, useState, type RefObject } from 'react';
import { activeSectionAt, groupOf, type SettingsNavGroup } from '../../lib/settingsNav';

/** Below the sticky app header (and, on mobile, the group tabs). */
const ACTIVE_LINE_PX = 140;

/** How long a jump may take to bring its section on screen before the pin lapses. */
const PIN_TRAVEL_MS = 2000;

/**
 * Which section the reader is in, following scroll anywhere on the page.
 * `select` jumps to a section and keeps it highlighted while the smooth
 * scroll travels there and while it stays on screen, even when it's too
 * short to reach the top (the last few at the bottom). Once it has been on
 * screen and leaves, normal tracking resumes.
 */
function useActiveSection(ids: string[]) {
  const [active, setActive] = useState(ids[0] ?? '');
  const pinned = useRef<{ id: string; reached: boolean; at: number } | null>(null);

  useEffect(() => {
    let frame = 0;
    const update = (target: EventTarget | null) => {
      const scroller =
        target instanceof Element ? target : (document.scrollingElement ?? document.documentElement);
      const tops = ids.map((id) => document.getElementById(id)?.getBoundingClientRect().top ?? Infinity);
      const atBottom = scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 4;
      const pin = pinned.current;
      if (pin) {
        const top = tops[ids.indexOf(pin.id)] ?? Infinity;
        const onScreen = top >= -1 && top < window.innerHeight;
        if (onScreen) pin.reached = true;
        const travelling = !pin.reached && performance.now() - pin.at < PIN_TRAVEL_MS;
        if (onScreen || travelling) {
          setActive(pin.id);
          return;
        }
        pinned.current = null;
      }
      setActive(activeSectionAt(ids, tops, ACTIVE_LINE_PX, atBottom));
    };
    const onScroll = (e: Event) => {
      if (e.target instanceof Element && !e.target.contains(document.getElementById(ids[0] ?? ''))) return;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => update(e.target));
    };
    // Capture, so it hears whichever element is doing the scrolling.
    window.addEventListener('scroll', onScroll, { capture: true, passive: true });
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener('scroll', onScroll, { capture: true });
    };
  }, [ids]);

  const select = useCallback((id: string) => {
    pinned.current = { id, reached: false, at: performance.now() };
    setActive(id);
    document.getElementById(id)?.scrollIntoView?.({ behavior: 'smooth', block: 'start' });
  }, []);

  return [active, select] as const;
}

/**
 * Where the fixed navigation starts, in pixels from the viewport top: just
 * below the app's sticky header, or lower while the top of the Settings page
 * (the parent of `rail`, the desktop nav) is still further down — the alert and update banners sit between
 * the header and the page and scroll away normally, so the navigation must
 * not cover them. Measured, because the header wraps on narrow phones and
 * grows with the safe-area inset. `null` until measured.
 */
function useNavTop(rail: RefObject<HTMLElement | null>): number | null {
  const [top, setTop] = useState<number | null>(null);
  useEffect(() => {
    const header = document.querySelector('header');
    const page = rail.current?.parentElement ?? null;
    let frame = 0;
    const measure = () => {
      const headerBottom = header ? header.getBoundingClientRect().bottom : 0;
      const pageTop = page?.getBoundingClientRect().top ?? 0;
      setTop(Math.max(0, Math.round(Math.max(headerBottom, pageTop))));
    };
    const onChange = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    measure();
    const observer = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(onChange) : null;
    if (header) observer?.observe(header);
    if (page) observer?.observe(page);
    window.addEventListener('resize', onChange);
    window.addEventListener('scroll', onChange, { capture: true, passive: true });
    return () => {
      cancelAnimationFrame(frame);
      observer?.disconnect();
      window.removeEventListener('resize', onChange);
      window.removeEventListener('scroll', onChange, { capture: true });
    };
  }, [rail]);
  return top;
}

/**
 * Settings navigation: a fixed rail listing every section under its group
 * on wide screens, and a fixed strip of group tabs on narrow ones, both
 * below the header and any banners. (Sticky can't work: the app's <main> is
 * an overflow container while the window does the scrolling.)
 */
export default function SettingsNav({ groups }: { groups: SettingsNavGroup[] }) {
  const [ids] = useState(() => groups.flatMap((g) => g.sections.map((s) => s.id)));
  const [active, select] = useActiveSection(ids);
  const activeGroup = groupOf(groups, active);
  const railRef = useRef<HTMLElement>(null);
  const navTop = useNavTop(railRef);

  return (
    <>
      <nav
        aria-label="Settings sections"
        // One row with every group visible: they share the width equally.
        style={{
          gridTemplateColumns: `repeat(${groups.length}, minmax(0, 1fr))`,
          ...(navTop != null ? { top: navTop } : {}),
        }}
        className="md:hidden fixed inset-x-0 top-[calc(env(safe-area-inset-top,0px)+3.5rem+1px)] z-20 px-2 py-2 bg-bg-base/90 backdrop-blur-md border-b border-white/5 grid gap-1"
      >
        {groups.map((g) => {
          const current = g === activeGroup;
          return (
            <button
              key={g.label}
              type="button"
              aria-current={current ? 'true' : undefined}
              onClick={() => select(g.sections[0]!.id)}
              className={`min-w-0 rounded-lg px-1 py-1.5 text-[11px] leading-tight text-center font-sans font-medium transition-colors ${
                current ? 'bg-accent text-on-accent' : 'bg-bg-surface text-text-secondary hover:text-text-primary'
              }`}
            >
              {g.label}
            </button>
          );
        })}
      </nav>

      {/* Holds the tab bar's space, since a fixed bar takes none. */}
      <div aria-hidden="true" className="md:hidden h-14" />

      <nav ref={railRef} aria-label="Settings" className="hidden md:block">
        {/* Fixed with no left offset: it stays in its grid column but no
            longer scrolls. */}
        <div
          // 24px matches the page's top padding, so the rail lines up with
          // the first section before any scrolling.
          style={navTop != null ? { top: navTop + 24, maxHeight: `calc(100vh - ${navTop + 96}px)` } : undefined}
          className="fixed top-24 w-48 max-h-[calc(100vh-11rem)] overflow-y-auto flex flex-col gap-4"
        >
          {groups.map((g) => (
            <div key={g.label} className="flex flex-col gap-0.5">
              <span className="px-3 pb-1 text-[11px] font-sans font-semibold uppercase tracking-wide text-text-secondary/70">
                {g.label}
              </span>
              {g.sections.map((s) => {
                const current = s.id === active;
                return (
                  // A button, not an `#id` link: the app uses a hash router,
                  // so a link opened in a new tab would name a missing route.
                  <button
                    key={s.id}
                    type="button"
                    aria-current={current ? 'location' : undefined}
                    onClick={() => select(s.id)}
                    className={`text-left rounded-lg px-3 py-1.5 text-sm font-sans transition-colors ${
                      current
                        ? 'bg-accent/15 text-accent font-medium'
                        : 'text-text-secondary hover:text-text-primary hover:bg-bg-surface'
                    }`}
                  >
                    {s.label}
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      </nav>
    </>
  );
}
