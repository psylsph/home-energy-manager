import { TEXT_INPUT } from './styles';

/**
 * An IPv4 address input, with the Modbus port beside it when `port` is given
 * (developer mode), shared by the inverter and EV charger sections.
 */
export function HostField({
  label,
  host,
  onHost,
  invalid,
  placeholder,
  port,
  onPort,
  portTitle,
}: {
  label: string;
  host: string;
  onHost: (host: string) => void;
  invalid: boolean;
  placeholder: string;
  port?: number;
  onPort: (port: number) => void;
  portTitle: string;
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-text-secondary text-xs font-sans">{label}</span>
      <div className="flex gap-2">
        <input
          type="text"
          value={host}
          onChange={(e) => onHost(e.target.value)}
          placeholder={placeholder}
          aria-invalid={invalid}
          className={`min-w-0 flex-1 bg-bg-elevated text-text-primary rounded-lg px-3 py-2 text-sm font-mono border outline-none transition-colors ${
            invalid ? 'border-red-500 focus:border-red-400' : 'border-bg-elevated focus:border-accent'
          }`}
        />
        {port !== undefined && (
          <input
            type="number"
            value={port}
            onChange={(e) => onPort(Number(e.target.value))}
            title={portTitle}
            className={`w-[5.5em] shrink-0 ${TEXT_INPUT}`}
          />
        )}
      </div>
      {invalid && (
        <span className="text-red-400 text-xs font-sans">
          Must be four numbers separated by dots, e.g. <span className="font-mono">192.168.1.50</span>.
        </span>
      )}
    </label>
  );
}

/** Network-scan results, each with a "Use" button that fills in the form. */
export function DiscoveredList({
  title,
  error,
  items,
}: {
  title: string;
  error: string;
  items: { address: string; detail: string; use: () => void }[];
}) {
  return (
    <>
      {error && <p className="text-red-400 text-sm font-sans">{error}</p>}
      {items.length > 0 && (
        <div className="flex flex-col gap-2 mt-1">
          <span className="text-text-secondary text-xs font-sans">{title}</span>
          {items.map((item, i) => (
            <div key={i} className="bg-bg-elevated rounded-lg px-4 py-3 flex items-center justify-between">
              <div className="flex flex-col gap-0.5">
                <span className="text-text-primary text-sm font-mono">{item.address}</span>
                <span className="text-text-secondary text-xs font-sans">{item.detail}</span>
              </div>
              <button
                onClick={item.use}
                className="bg-accent/20 text-accent text-xs font-sans font-semibold px-3 py-1.5 rounded-md hover:bg-accent/30 transition-colors"
              >
                Use
              </button>
            </div>
          ))}
        </div>
      )}
    </>
  );
}
