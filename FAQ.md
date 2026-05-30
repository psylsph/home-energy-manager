# FAQ

Common problems and solutions for GivEnergy Local.

---

### The app shows "Waiting for data" and never loads

This usually means the app can't reach your inverter. Check the following:

1. **Inverter IP address** — Go to **Settings** and make sure the IP address matches your GivEnergy data adapter. You can click **Scan Network** to find it automatically.

2. **Same network** — Your computer/phone must be on the same WiFi or wired network as the inverter. VPNs, guest networks, and VLANs can block the connection.

3. **Firewall** — When the app first starts, your operating system may ask for permission to access the network. If you missed or denied this prompt:
   - **Windows**: Open **Windows Defender Firewall** → **Allow an app through firewall** → find **GivEnergy Local** and check both Private and Public networks. Or restart the app to trigger the prompt again.
   - **macOS**: Open **System Settings** → **Network** → **Firewall** → make sure GivEnergy Local is allowed.
   - **Linux**: Check `ufw status` or `iptables` — you may need to allow port 8899 outbound and port 7337 inbound.

4. **Restart the app** — Sometimes a clean restart resolves connection issues.

---

### I can't access the dashboard from my phone or tablet

The dashboard runs a local web server on port **7337**. To access it from another device:

1. Go to **Settings → Network Access** and note the URL shown (e.g. `http://192.168.1.x:7337`).
2. Open that URL in a browser on your other device.
3. If it doesn't load, check your firewall settings (see above).
4. Make sure both devices are on the same network.

---

### On macOS, the app says it "can't be opened because it is from an unidentified developer"

1. Right-click (or Control-click) the app and select **Open**.
2. Click **Open** again in the confirmation dialog.
3. This only needs to be done once — macOS will remember your choice.

If the right-click → Open method doesn't work:

1. Open **System Settings** → **Privacy & Security**.
2. Scroll down to the **Security** section.
3. You should see a message saying "GivEnergy Local was blocked from opening."
4. Click **Open Anyway**.

> ⚠️ Only do this if you are comfortable trusting the app. GivEnergy Local is open source — you can [inspect the code](https://github.com/psylsph/givenergy-local) yourself.

---

### Which macOS download should I use?

Use the **x64 .dmg** download, even on Apple Silicon (M1/M2/M3/M4) Macs. The app is compiled as an x86_64 binary and runs via Apple's Rosetta 2 translation layer, which is automatic and seamless.

The **.deb** file is for Linux only.

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

Open an issue on [GitHub](https://github.com/psylsph/givenergy-local/issues) and we'll help you out.
