/**
 * Burger-menu navigation used with the house view (issue #422): a button in
 * the header's top-left that opens a drawer listing the same pages as the
 * bottom bar (from `visibleNavItems`).
 *
 * The drawer closes on choosing a page, Escape, or a tap outside; keeps
 * focus inside while open; and returns focus to the button when it closes.
 * Open state is "opened on page X" and counts as open only while X is still
 * the current page, so browser Back/Forward closes it too.
 */
import { useEffect, useId, useRef, useState, type ComponentType, type CSSProperties, type KeyboardEvent } from 'react';
import { createPortal } from 'react-dom';
import { NavLink, useLocation } from 'react-router';
import type { NavItem } from '../lib/navItems';

export interface NavDrawerItem extends NavItem {
  icon: ComponentType;
}

function MenuIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2} aria-hidden="true">
      <path strokeLinecap="round" d="M4 7h16M4 12h12M4 17h16" />
    </svg>
  );
}

function CloseIcon() {
  return (
    <svg className="w-5 h-5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2} aria-hidden="true">
      <path strokeLinecap="round" d="M6 6l12 12M18 6L6 18" />
    </svg>
  );
}

const FOCUSABLE = 'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])';

export default function NavDrawer({ items }: { items: NavDrawerItem[] }) {
  const { pathname } = useLocation();
  const [openedOn, setOpenedOn] = useState<string | null>(null);
  // Navigated away while open (Back/Forward, a link elsewhere): forget the
  // stale page so returning to it doesn't reopen the drawer. Adjusting state
  // during render is React's supported pattern for this; no effect needed.
  if (openedOn !== null && openedOn !== pathname) setOpenedOn(null);
  const open = openedOn === pathname;
  const buttonRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const wasOpen = useRef(false);
  const drawerId = useId();

  const close = () => setOpenedOn(null);

  // Focus moves into the drawer on open (the current page, else the first
  // link) and back to the button on close, however it was closed.
  useEffect(() => {
    if (open) {
      const panel = panelRef.current;
      const target = panel?.querySelector<HTMLElement>('[aria-current="page"]')
        ?? panel?.querySelector<HTMLElement>(FOCUSABLE);
      target?.focus();
    } else if (wasOpen.current) {
      buttonRef.current?.focus();
    }
    wasOpen.current = open;
  }, [open]);

  // Escape closes; the page behind doesn't scroll while the drawer is open.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        setOpenedOn(null);
      }
    };
    document.addEventListener('keydown', onKey);
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => {
      document.removeEventListener('keydown', onKey);
      document.body.style.overflow = prevOverflow;
    };
  }, [open]);

  // Keep Tab / Shift+Tab inside the drawer.
  const onPanelKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'Tab') return;
    const panel = e.currentTarget;
    // The Close button is always there, so the list is never empty.
    const focusable = Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE));
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || !panel.contains(active))) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (active === last || !panel.contains(active))) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        onClick={() => setOpenedOn(open ? null : pathname)}
        aria-label="Menu"
        aria-expanded={open}
        aria-controls={drawerId}
        className="flex items-center justify-center w-9 h-9 rounded-full border border-white/10 bg-bg-elevated text-accent transition hover:bg-bg-elevated/80 focus:outline-none focus:ring-2 focus:ring-accent/60"
      >
        <MenuIcon />
      </button>

      {/* Portalled to <body>: the header's backdrop blur would otherwise be
          the containing block for this fixed layer and clip it to the header. */}
      {open && createPortal(
        <div className="fixed inset-0 z-40" data-testid="nav-drawer-layer">
          <div
            className="absolute inset-0 bg-black/50"
            data-testid="nav-drawer-backdrop"
            aria-hidden="true"
            onClick={close}
          />
          <div
            ref={panelRef}
            id={drawerId}
            role="dialog"
            aria-modal="true"
            aria-label="Pages"
            onKeyDown={onPanelKeyDown}
            className="absolute inset-y-0 left-0 w-64 max-w-[80vw] bg-bg-surface border-r border-white/10 shadow-2xl flex flex-col pt-[calc(env(safe-area-inset-top,0px)+0.75rem)] pb-safe"
          >
            <div className="flex items-center justify-between px-4 pb-3 border-b border-white/5">
              <span className="text-sm font-semibold text-text-primary">Pages</span>
              <button
                type="button"
                onClick={close}
                aria-label="Close Menu"
                className="flex items-center justify-center w-8 h-8 rounded-full text-text-secondary hover:text-text-primary focus:outline-none focus:ring-2 focus:ring-accent/60"
              >
                <CloseIcon />
              </button>
            </div>
            <nav aria-label="Pages" className="flex-1 overflow-y-auto py-2">
              {items.map(({ to, label, icon: Icon, accent }) => (
                <NavLink
                  key={to}
                  to={to}
                  end={to === '/'}
                  onClick={close}
                  style={accent ? ({ '--nav-accent': accent } as CSSProperties) : undefined}
                  className={({ isActive }) =>
                    `flex items-center gap-3 mx-2 px-3 py-2.5 rounded-lg text-sm font-medium transition-colors focus:outline-none focus:ring-2 focus:ring-accent/60 ${
                      isActive
                        ? `bg-bg-elevated ${accent ? 'text-[var(--nav-accent)]' : 'text-accent'}`
                        : 'text-text-secondary hover:text-text-primary hover:bg-bg-elevated/60'
                    }`
                  }
                >
                  <Icon />
                  <span>{label}</span>
                </NavLink>
              ))}
            </nav>
          </div>
        </div>,
        document.body,
      )}
    </>
  );
}
