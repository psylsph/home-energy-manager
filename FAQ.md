# FAQ

Common problems and solutions for Home Energy Manager.

---

### The app shows "Waiting for data" and never loads

This usually means the app can't reach your inverter. Check the following:

1. **Inverter IP address** — Go to **Settings** and make sure the IP address matches your GivEnergy data adapter. You can click **Scan Network** to find it automatically.

2. **Same network** — Your computer/phone must be on the same WiFi or wired network as the inverter. VPNs, guest networks, and VLANs can block the connection.

3. **Firewall** — When the app first starts, your operating system may ask for permission to access the network. If you missed or denied this prompt:
   - **Windows**: Open **Windows Defender Firewall** → **Allow an app through firewall** → find **Home Energy Manager** and check both Private and Public networks. Or restart the app to trigger the prompt again.
   - **macOS**: Open **System Settings** → **Network** → **Firewall** → make sure Home Energy Manager is allowed.
   - **Linux**: Check `ufw status` or `iptables` — you may need to allow port 8899 outbound and port 7337 inbound.

4. **Restart the app** — Sometimes a clean restart resolves connection issues.

---

### The app can connect but the official GivEnergy app can't (WiFi-UART mode)

If you've recently factory-reset your data adapter (the small WiFi or Ethernet
dongle connected to your inverter), you may need to check the **WiFi-UART**
setting on the dongle's internal web page:

1. Find your inverter's IP address and enter it in a browser (you may need to
   be on the same network with the dongle in access-point mode).
2. Log in to the dongle's configuration page (default credentials are often
   printed on the dongle itself).
3. Look for a **WiFi-UART** or **Working Mode** setting.
4. Make sure it's set to **Server** — not **Client**.

When the dongle is in **Client** mode, the app (and the official app) can't
communicate with the inverter even though the cloud portal shows it as online.
This is a common issue after a factory reset, as the default may be Client.

---

### I can't access the dashboard from my phone or tablet

The dashboard runs a local web server on port **7337**. To access it from another device:

1. Go to **Settings → Network Access** and note the URL shown (e.g. `http://192.168.1.x:7337`).
2. Open that URL in a browser on your other device.
3. If it doesn't load, check your firewall settings (see above).
4. Make sure both devices are on the same network.

---

### I updated the app, but Settings still shows the old version

This usually means an older copy of the app is still running in the background.
Home Energy Manager runs a local web server on port **7337**; if the old app
still owns that port, a newly opened app window can show the old app's frontend
and version number.

**Fix**:

1. Close Home Energy Manager / GivEnergy Local.
2. Check Activity Monitor or `ps` for any remaining `givenergy-local` process and stop it.
3. Reopen the app. If unsure, reboot and then open the app again.

If the version still shows as old after the above:

4. **Hard refresh** the app window with `Cmd+Shift+R`.
5. Or clear the Tauri webview cache:
   ```bash
   rm -rf ~/Library/Caches/com.givenergy.local/
   ```
   Then reopen the app.

This last step removes any stale Service Worker from a previous version
that may be intercepting requests and serving cached old JS.

---

### On macOS, the app says it "can't be opened because it is from an unidentified developer"

1. Right-click (or Control-click) the app and select **Open**.
2. Click **Open** again in the confirmation dialog.
3. This only needs to be done once — macOS will remember your choice.

If the right-click → Open method doesn't work:

1. Open **System Settings** → **Privacy & Security**.
2. Scroll down to the **Security** section.
3. You should see a message saying "Home Energy Manager was blocked from opening."
4. Click **Open Anyway**.

> ⚠️ Only do this if you are comfortable trusting the app. Home Energy Manager is open source — you can [inspect the code](https://github.com/psylsph/home-energy-manager) yourself.

### On macOS 26.5+, the app launches but the web UI never loads

This can happen for two reasons:

**1. App is installed in /Applications** — macOS 26.5 blocks ad-hoc signed
binaries from running inside `/Applications`, even when launched directly from
terminal (not just `open`). **Fix**: Move the .app to your Desktop or home
folder instead.

```bash
mv "/Applications/Home Energy Manager.app" ~/Desktop/
```

Then launch from there — it will work as long as the binary isn't in a system-
protected directory.

**2. Gatekeeper blocking `open`** — When you use `open` (or double-click the
app in Finder), macOS may silently block the web server. **Workaround**: Run
the binary directly:

```bash
"$HOME/Desktop/Home Energy Manager.app/Contents/MacOS/givenergy-local"
```

Or use the `launch.command` convenience script from the project root.

> Note: The old `spctl --add` command-line workaround is no longer supported on
> macOS 26.5.

**3. Running the wrong architecture on Apple Silicon** — The **x64 (Intel) .dmg**
crashes silently under Rosetta on macOS 26.5+ with no error output. Reinstall
using the **aarch64.dmg** instead.

---

### Which macOS download should I use?

On **Apple Silicon (M1/M2/M3/M4/M5)** Macs, use the **aarch64 .dmg** download.
The x64 (Intel) .dmg may crash silently on macOS 26.5+ under Rosetta.

On **Intel** Macs, use the **x64 .dmg** download.

---

### The network scan doesn't find my inverter

- Make sure your inverter's data adapter is powered on and connected to your router.
- The scan probes port **8899** and verifies the GivEnergy Modbus protocol — only genuine GivEnergy devices will appear.
- Try entering the IP address manually if you know it (you can find it in your router's device list).

---

### The app was working but stopped updating

- The connection to the inverter may have dropped. The app reconnects automatically — wait 30 seconds.
- Check that your inverter's data adapter hasn't rebooted or lost its network connection.
- If the problem persists, restart the app.

---

### How do I find my inverter's IP address?

1. Open your router's admin page (usually `http://192.168.0.1` or `http://192.168.1.1`).
2. Look for a device list or DHCP client list.
3. Search for a device named "GivEnergy" or look for the data adapter's MAC address (printed on the device).
4. The IP address will be something like `192.168.1.x`.

Alternatively, use the **Scan Network** button on the Settings page.

---

### Something else?

Open an issue on [GitHub](https://github.com/psylsph/home-energy-manager/issues) and we'll help you out.
