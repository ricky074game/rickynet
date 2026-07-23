# RickyNet Runbook — end-user setup

Goal: your Windows PC browses the internet through the iPhone's **cellular**
data, over the USB cable, without Personal Hotspot.

You need: the two CI artifacts (`RickyNet-unsigned.ipa`, and the `RickyNet-windows`
zip containing `rickynet.exe` + `wintun.dll`), a Lightning/USB-C cable, and the
iPhone owner's own Apple ID.

---

## USB setup (the main path), in order

### 1. On the Windows PC: install Apple's USB device service
Install **iTunes** *or* Apple's **"Apple Devices"** app (Microsoft Store). Either
one installs the **Apple Mobile Device Service** (`usbmuxd`) that RickyNet talks
to on `127.0.0.1:27015`.
> The Wi-Fi transport (below) does **not** need this — use it to test first if you
> want to skip the Apple install.

### 2. Install RickyNet on the iPhone via SideStore/AltStore
Sideload **`RickyNet-unsigned.ipa`** with **SideStore** (or AltStore) signed with
the iPhone owner's **own Apple ID** (free provisioning is fine).
> Free Apple IDs expire the signature after **~7 days** — SideStore re-signs it
> for you; just keep SideStore set up. RickyNet uses only plain client/server
> sockets, so it needs **no special entitlement** and installs fine this way.

### 3. Plug the iPhone into the PC and tap **Trust**
Connect the cable. On the phone, tap **Trust This Computer** and enter the
passcode if prompted.

### 4. Open RickyNet on the iPhone and tap **Start**
- The status light turns **green** and the screen stays awake.
- **Keep RickyNet in the foreground.** iOS kills background apps — if you leave
  the app or lock the phone, the link drops. (Leave "Accept over Wi-Fi (LAN)"
  **off** for USB.)

### 5. On the PC, run RickyNet as Administrator
Unzip `RickyNet-windows.zip` (keep `rickynet.exe` and `wintun.dll` **together**).
Double-click **`rickynet.exe`** → accept the **UAC** prompt (it needs Admin to
create the network adapter and set routes).

In the window:
1. Leave the link set to **USB**.
2. Click the big **Connect** button.
3. Watch the state go **Connecting… → Connected** (green). The log shows
   `adapter up → connected to device → routes set`.

> Prefer the command line? `rickynet.exe --headless --transport usb`
> (run from an **elevated** terminal).

### 6. Verify, and how to stop
- Open a browser and load any site.
- Optional: `ping 1.1.1.1` (RickyNet re-originates ICMP over cellular).
- Confirm it's really the phone's data (turn the PC's Wi-Fi off — you should stay
  online through the phone).

**To stop:** click **Disconnect** in RickyNet (routes are restored and the adapter
removed), then tap **Stop** in the phone app. Closing the window minimizes to the
system tray; use the tray menu's **Quit** to exit fully (which also disconnects).

---

## Wi-Fi setup (development / no-Apple-service path)

Use this to test the whole system without iTunes/usbmux. Both devices must be on
the **same LAN**.

1. On the iPhone, enable **"Accept over Wi-Fi (LAN)"**, then tap **Start**. Note
   the phone's LAN IP (Settings → Wi-Fi → (i) → IP Address), e.g. `192.168.1.42`.
2. On the PC (as Administrator):
   - GUI: pick **Wi-Fi**, enter the phone IP, click **Connect**.
   - CLI: `rickynet.exe --headless --transport wifi --phone-ip 192.168.1.42`
3. RickyNet pins a host route for the phone's IP off-tunnel (so the link to the
   phone isn't captured) and routes everything else through it.

> Default listener port is **27600** on both ends (`--port` to change).

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| UAC prompt declined / "Administrator required" | Re-launch `rickynet.exe` and accept UAC. It won't fake a connected state without Admin. |
| `no USB iPhone found via usbmux` | Install iTunes / Apple Devices (step 1), replug, tap **Trust**, and make sure **RickyNet is open and Started** on the phone. |
| `usbmux Connect failed (Number=3 …)` | Nothing is listening on the phone port — open RickyNet and tap **Start** first. |
| Connects, but no internet | Make sure the phone has cellular signal and mobile data on; RickyNet needs a live `pdp_ip0`. On the Simulator/Wi-Fi-only devices there is no cellular interface. |
| `could not load wintun.dll` | Keep `wintun.dll` in the **same folder** as `rickynet.exe`. |
| Wi-Fi mode can't reach the phone | Same subnet? Phone's firewall/app is Started with **Accept over Wi-Fi** on? Correct `--phone-ip`? |
| Link drops after a bit | The iPhone app was backgrounded or the phone locked — bring RickyNet back to the foreground and Start again. |
| Slow | Expected — userspace re-origination on the phone. Fine for browsing, not line-rate. |

## Getting logs (send these when reporting a problem)

Both sides log **everything** by default (debug level) — always grab BOTH files:

- **Windows:** `%LOCALAPPDATA%\RickyNet\rickynet.log` (falls back to the folder
  next to `rickynet.exe`, then the temp dir). The GUI shows the path under the
  log panel and has **Open log file** / **Copy** buttons. Rotates to
  `rickynet.old.log` at ~5 MB — include that too if it exists.
  `RUST_LOG` (e.g. `RUST_LOG=trace`) overrides verbosity.
- **iPhone:** in the app tap **Logs** → the share icon to send `rickynet.log`
  (AirDrop/Messages/Mail). The same file is also visible in the **Files app →
  On My iPhone → RickyNet**. Rotates to `rickynet.old.log` at ~4 MB.

Reproduce the problem first, then share the logs right away (connect attempt,
flow errors, and a 15-second traffic heartbeat are all recorded).

## What "clean shutdown" does
Disconnecting removes the two split-default routes (and, for Wi-Fi, the phone's
`/32` carve-out) and drops the Wintun adapter, which restores your normal routing
and DNS. If `rickynet.exe` is killed hard, re-running it once and disconnecting
cleanly will tidy up; the adapter is reused rather than duplicated.
