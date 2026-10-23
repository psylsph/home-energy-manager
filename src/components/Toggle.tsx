/**
 * An on/off switch. The label is required: every switch must have an
 * accessible name, and the type system now enforces it. It is focusable and
 * flips on Space or Enter as well as on click. While `disabled` it is dimmed,
 * announced as disabled, and ignores clicks and keys.
 */
export default function Toggle({
  checked,
  onChange,
  ariaLabel,
  disabled,
  testId,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  ariaLabel: string;
  disabled?: boolean;
  testId?: string;
}) {
  const flip = () => {
    if (!disabled) onChange(!checked);
  };
  return (
    <div
      className={`relative shrink-0 ${disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer'}`}
      role="switch"
      aria-label={ariaLabel}
      aria-checked={checked}
      aria-disabled={disabled}
      data-testid={testId}
      tabIndex={0}
      onClick={flip}
      onKeyDown={(e) => {
        if (e.key === ' ' || e.key === 'Enter') {
          // Space would otherwise scroll the page.
          e.preventDefault();
          flip();
        }
      }}
    >
      <div className={`w-10 h-5 rounded-full transition-colors ${checked ? 'bg-accent/40' : 'bg-bg-elevated'}`} />
      <div className={`absolute left-0.5 top-0.5 w-4 h-4 rounded-full transition-all ${checked ? 'translate-x-5 bg-accent' : 'bg-text-secondary'}`} />
    </div>
  );
}
