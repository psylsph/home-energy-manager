import { openExternal } from '../../lib/openExternal';
import { SettingsSection } from './SettingsSection';

const WRITE_UP_URL =
  'https://www.pentestpartners.com/security-blog/givenergy-enters-administration-legacy-home-batteries-still-expose-customer-networks/';

const CHECKLIST: { title: string; detail: string }[] = [
  {
    title: "Change the dongle's Wi-Fi password",
    detail: "It's often just the inverter's serial, which is in the network name.",
  },
  {
    title: "Change the dongle's web admin login",
    detail: 'Its web page and Telnet logins are often admin and admin.',
  },
  {
    title: 'Put the inverter on its own network',
    detail: 'A separate VLAN or guest network keeps other devices away from it.',
  },
  {
    title: "Don't port-scan the network it's on",
    detail: 'A scan can crash its Wi-Fi module and reset it to factory defaults.',
  },
];

/**
 * Settings → Dongle Security (issue #397): passive advice from Pen Test
 * Partners' write-up. Nothing here runs a check, and HEM never probes the
 * dongle's other ports: that is exactly what crashes it.
 */
export default function DongleSecuritySection({ id }: { id?: string }) {
  return (
    <SettingsSection
      id={id}
      title="Dongle Security"
      description="Four things to check on the Wi-Fi dongle. HEM can't check them for you."
      help="Pen Test Partners found that port-scanning a GivEnergy dongle can crash its Wi-Fi module and reset it to factory defaults, wiping the charge and discharge schedule, and that its default logins are weak. With GivEnergy in administration no firmware fix is coming. HEM only talks to the inverter on port 8899 and never probes the dongle's other ports. If a reset does happen, HEM keeps the last schedule it saw and offers to restore it on the Control page."
      action={
        <button
          type="button"
          onClick={() => void openExternal(WRITE_UP_URL)}
          className="text-accent text-xs font-sans underline hover:opacity-80 transition-opacity"
        >
          Pen Test Partners Write-Up ↗
        </button>
      }
    >
      <ul className="flex flex-col gap-3">
        {CHECKLIST.map(({ title, detail }) => (
          <li key={title} className="flex gap-3">
            <span aria-hidden="true" className="text-accent font-sans text-sm leading-5">
              ☐
            </span>
            <div className="flex flex-col">
              <span className="text-text-primary text-sm font-sans">{title}</span>
              <span className="text-text-secondary text-xs font-sans">{detail}</span>
            </div>
          </li>
        ))}
      </ul>
    </SettingsSection>
  );
}
