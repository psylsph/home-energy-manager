import { useId, useState, type ReactNode } from 'react';
import HelpTip, { HeadingWithHelp } from '../HelpTip';

const H2 = 'text-text-primary text-lg font-semibold font-sans';
const H3 = 'text-text-primary text-sm font-sans font-medium';

/** One card on the Settings page: the `<section>` and its `<h2>` header. */
export function SettingsSection({
  id,
  title,
  action,
  description,
  help,
  inert,
  children,
}: {
  /** Anchor for in-page navigation. */
  id?: string;
  title: string;
  /** Shown at the right of the header, e.g. a "Setup guide ↗" link. */
  action?: ReactNode;
  /** One line under the title; keep it short and put the detail in `help`. */
  description?: ReactNode;
  /** The full explanation, behind an ⓘ next to the title. */
  help?: ReactNode;
  inert?: boolean;
  children?: ReactNode;
}) {
  return (
    <section
      id={id}
      inert={inert}
      className="bg-bg-surface rounded-xl p-5 flex flex-col gap-4 scroll-mt-32 md:scroll-mt-24"
    >
      {action ? (
        <div className="flex items-center justify-between gap-3">
          <HeadingWithHelp as="h2" className={H2} title={title} help={help} />
          {action}
        </div>
      ) : (
        <HeadingWithHelp as="h2" className={H2} title={title} help={help} />
      )}
      {description && <p className="text-text-secondary text-xs font-sans -mt-2">{description}</p>}
      {children}
    </section>
  );
}

/** A bordered group inside a section, with an `<h3>` header. */
export function SettingsGroup({
  title,
  action,
  description,
  help,
  helpLabel,
  children,
}: {
  title: ReactNode;
  action?: ReactNode;
  /** One line under the title; keep it short and put the detail in `help`. */
  description?: ReactNode;
  /** The full explanation, behind an ⓘ next to the title. */
  help?: ReactNode;
  /** Names the ⓘ when `title` isn't plain text. */
  helpLabel?: string;
  children?: ReactNode;
}) {
  return (
    <div className="border border-white/5 rounded-xl p-4 flex flex-col gap-3">
      {action ? (
        <div className="flex items-center justify-between">
          <HeadingWithHelp as="h3" className={H3} title={title} help={help} helpLabel={helpLabel} />
          {action}
        </div>
      ) : (
        <HeadingWithHelp as="h3" className={H3} title={title} help={help} helpLabel={helpLabel} />
      )}
      {description && <p className="text-text-secondary text-xs font-sans">{description}</p>}
      {children}
    </div>
  );
}

/** A labelled toggle row: name and optional hint on the left, control on the right. */
export function SettingsRow({
  label,
  hint,
  help,
  helpLabel,
  strong,
  children,
}: {
  label: ReactNode;
  /** One line under the label; keep it short and put the detail in `help`. */
  hint?: ReactNode;
  /** The full explanation, behind an ⓘ next to the label. */
  help?: ReactNode;
  /** Names the ⓘ when `label` isn't plain text. */
  helpLabel?: string;
  /** Bolder label, for a row that stands alone rather than in a list. */
  strong?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className={`text-text-primary text-sm font-sans${strong ? ' font-medium' : ''}`}>
        {label}
        {help && (
          <>
            {' '}
            <HelpTip label={helpLabel ?? (typeof label === 'string' ? label : 'this setting')}>{help}</HelpTip>
          </>
        )}
        {hint && <span className="block text-text-secondary text-xs font-sans">{hint}</span>}
      </span>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

/**
 * Rarely changed settings, folded away until asked for (issue #402). The
 * fields stay owned by the section, so folding them never loses an edit.
 */
export function AdvancedSettings({ children }: { children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const panelId = useId();
  return (
    <div className="flex flex-col gap-3">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => setOpen((o) => !o)}
        className="self-start flex items-center gap-1.5 text-text-secondary hover:text-text-primary text-xs font-sans font-medium uppercase tracking-wide transition-colors"
      >
        <span aria-hidden="true" className={`inline-block transition-transform ${open ? 'rotate-90' : ''}`}>▸</span>
        Advanced
      </button>
      {open && (
        <div id={panelId} className="flex flex-col gap-4 border-l-2 border-white/5 pl-4">
          {children}
        </div>
      )}
    </div>
  );
}
