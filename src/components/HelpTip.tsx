import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from 'react';

const GAP = 6;
const MARGIN = 8;
/** How long a hover tip waits after the mouse leaves, so it can cross the gap onto the tip. */
const HOVER_CLOSE_MS = 200;

/**
 * A small ⓘ button that reveals a longer explanation (issue #423), so the
 * page keeps a one-line hint and the detail is one hover, focus or tap away.
 *
 * - Mouse: opens on hover and stays while the mouse is over the icon or the
 *   tip; a click pins it open until clicked again or a click elsewhere.
 * - Keyboard: opens on focus; Enter/Space pins, Escape or Tab away closes.
 * - Touch: a tap opens it, a second tap or a tap elsewhere closes it.
 *
 * The text stays in the DOM while closed (`hidden`), so the button can point
 * at it with `aria-describedby` and screen readers announce it on focus.
 */
export default function HelpTip({
  label,
  children,
  className = '',
}: {
  /** What the help is about; the button is named "More about {label}". */
  label: string;
  children: ReactNode;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const pinned = useRef(false);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const tipRef = useRef<HTMLSpanElement>(null);
  const tipId = useId();
  const hoverTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancelHoverClose = useCallback(() => {
    if (hoverTimer.current !== null) clearTimeout(hoverTimer.current);
    hoverTimer.current = null;
  }, []);

  const close = useCallback(() => {
    cancelHoverClose();
    pinned.current = false;
    setOpen(false);
  }, [cancelHoverClose]);

  // Below the icon, kept inside the viewport (above the icon when there's no
  // room below). The tip is fixed-position, so it is placed again whenever
  // the page scrolls or resizes rather than being left behind.
  const place = useCallback(() => {
    const button = buttonRef.current;
    const tip = tipRef.current;
    if (!button || !tip) return;
    const b = button.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const left = Math.max(MARGIN, Math.min(b.left + b.width / 2 - t.width / 2, vw - t.width - MARGIN));
    const below = b.bottom + GAP;
    const top = below + t.height > vh - MARGIN && b.top - GAP - t.height >= MARGIN ? b.top - GAP - t.height : below;
    setPos({ top, left });
  }, []);

  useLayoutEffect(() => {
    if (open) place();
  }, [open, place]);

  useEffect(() => cancelHoverClose, [cancelHoverClose]);

  // A tap or click anywhere else closes it, as does Escape.
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close();
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKey);
    window.addEventListener('scroll', place, true);
    window.addEventListener('resize', place);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKey);
      window.removeEventListener('scroll', place, true);
      window.removeEventListener('resize', place);
    };
  }, [open, close, place]);

  return (
    <span
      ref={wrapRef}
      className={`inline-flex align-middle ${className}`}
      // On the wrapper so the mouse can move from the icon onto the tip.
      onPointerEnter={(e) => {
        if (e.pointerType !== 'mouse') return;
        cancelHoverClose();
        setOpen(true);
      }}
      onPointerLeave={(e) => {
        if (e.pointerType !== 'mouse' || pinned.current) return;
        cancelHoverClose();
        hoverTimer.current = setTimeout(() => {
          hoverTimer.current = null;
          // Still focused from the keyboard: leave it to blur or Escape.
          if (!pinned.current && document.activeElement !== buttonRef.current) setOpen(false);
        }, HOVER_CLOSE_MS);
      }}
    >
      <button
        ref={buttonRef}
        type="button"
        aria-label={`More about ${label}`}
        aria-expanded={open}
        aria-describedby={tipId}
        onFocus={() => setOpen(true)}
        onBlur={(e) => {
          // Close when focus moves on to another control (Tab, or a click on
          // something focusable). A click on the tip's own text leaves focus
          // nowhere (relatedTarget null); a click elsewhere is handled by the
          // pointerdown listener, so neither closes it here.
          const next = e.relatedTarget as Node | null;
          if (next && !wrapRef.current?.contains(next)) close();
        }}
        onClick={(e) => {
          // Inside a <label> or a clickable row the tap is only for the tip.
          e.preventDefault();
          e.stopPropagation();
          if (pinned.current) {
            close();
          } else {
            pinned.current = true;
            setOpen(true);
          }
        }}
        className="inline-flex items-center justify-center w-4 h-4 rounded-full text-[11px] leading-none text-text-secondary hover:text-text-primary focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
      >
        <span aria-hidden="true">ⓘ</span>
      </button>
      <span
        ref={tipRef}
        id={tipId}
        role="tooltip"
        hidden={!open}
        style={pos ? { top: pos.top, left: pos.left } : { top: 0, left: 0, visibility: 'hidden' }}
        className="fixed z-50 block w-72 max-w-[calc(100vw-1rem)] rounded-lg border border-white/10 bg-bg-elevated px-3 py-2 text-xs font-normal normal-case tracking-normal text-left text-text-primary shadow-lg whitespace-normal"
      >
        {children}
      </span>
    </span>
  );
}

/**
 * A heading with an ⓘ beside it. The ⓘ sits outside the heading element so
 * the heading's accessible name stays its title (issue #423).
 */
export function HeadingWithHelp({
  as: Tag,
  id,
  className,
  title,
  help,
  helpLabel,
}: {
  as: 'h2' | 'h3';
  id?: string;
  className: string;
  title: ReactNode;
  help?: ReactNode;
  /** Names the ⓘ; defaults to the title when that is plain text. */
  helpLabel?: string;
}) {
  const heading = (
    <Tag id={id} className={className}>
      {title}
    </Tag>
  );
  if (!help) return heading;
  return (
    <div className="flex items-center gap-1.5">
      {heading}
      <HelpTip label={helpLabel ?? (typeof title === 'string' ? title : 'this setting')}>{help}</HelpTip>
    </div>
  );
}
