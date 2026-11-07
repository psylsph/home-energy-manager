import HelpTip from './HelpTip';

/**
 * The login the dongle's web page asks for. These come from GivEnergy's and
 * installers' guides rather than from testing here, so they are things to
 * "try", not promises.
 */
export default function DongleLoginTip() {
  return (
    <HelpTip label="the dongle login">
      The page asks for a login. Try <strong>admin</strong> as the username and{' '}
      <strong>admin</strong> as the password, unless you&apos;ve changed it yourself. If that
      fails, the newer AIO 2 and Gateway 2 dongles may want <strong>admin</strong> /{' '}
      <strong>123456</strong>, and some need the inverter serial number as the password.
    </HelpTip>
  );
}
