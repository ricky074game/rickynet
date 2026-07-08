<p align="center">
  <img src="../assets/icon.svg" width="140" alt="RickyNet icon">
</p>

# RickyNet

**Use an iPhone's cellular data as a Windows PC's internet connection — over the
USB cable — WITHOUT iOS Personal Hotspot.**

Because RickyNet never turns on Personal Hotspot, the desktop's traffic leaves
the phone as the RickyNet app's *own* cellular socket connections. To the
carrier that's ordinary phone-app data usage, so it doesn't draw down a
tethering/hotspot allotment. (It's still your normal cellular data — RickyNet
does not hide or discount usage, it just doesn't use the hotspot path.)

RickyNet is built entirely in GitHub Actions — a **macOS** runner builds the
unsigned iOS `.ipa`, a **Windows** runner builds `rickynet.exe`. You don't need a
Mac or an Apple Developer account: install the app with **SideStore/AltStore**
using your own Apple ID (free provisioning).

---

## How it works (architecture "B": an IP-level gateway)

The desktop captures **all** of its IP traffic on a virtual TUN interface
(Wintun) and ships raw IP packets over the cable to the phone. The **phone** runs
a userspace TCP/IP stack that terminates those flows and re-originates each as a
real cellular socket. This gives full internet fidelity — real TCP, UDP, and
ICMP/ping — unlike a TCP-only SOCKS proxy. The netstack lives **on the phone**;
the Windows side is a dumb TUN↔cable shuttle.

```
   ┌─────────────────────────── Windows PC (rickynet.exe, Admin) ───────────────────────────┐
   │  apps → OS routing → Wintun TUN ──read IP packet P──► frame [u16 len][P] ──► transport  │
   │                                                                               │         │
   │      ◄── write P to Wintun ◄── deframe ◄──────────────────────────────────────┤         │
   └───────────────────────────────────────────────────────────────────────────────┼────────┘
                                                                                    │
                       USB: loopback → Apple usbmux (127.0.0.1:27015) → cable       │  (or Wi-Fi: LAN TCP)
                                                                                    │
   ┌────────────────────────────────── iPhone (RickyNet app, foreground) ──────────┼────────┐
   │  socket server ──► inject P into userspace netstack (ipstack) ─────────────────┘        │
   │       │                                                                                  │
   │       ├─ new TCP flow to X:Y  → open a REAL TCP socket to X:Y, bound to pdp_ip0 (cell),  │
   │       │                          splice netstack-socket ⇄ real-socket both ways          │
   │       ├─ UDP flow (incl. DNS)  → real UDP socket per flow, bound to cellular             │
   │       └─ ICMP echo             → real ICMP datagram socket, bound to cellular            │
   │                                                                                          │
   │  return packets ── frame [u16 len][P] ──► back over the transport to Windows             │
   └──────────────────────────────────────────────────────────────────────────────────────────┘
```

### Data flow, precisely
1. Windows reads IP packet `P` from Wintun → frames it as `[u16 big-endian length][P]` → writes it to the transport stream.
2. The phone reads `[len][P]`, injects `P` into the userspace stack.
3. For each new **TCP** flow to `X:Y`, the phone opens a **real OS socket** to `X:Y` bound to the cellular interface (`IP_BOUND_IF` → `pdp_ip0`) and splices the two sockets.
4. For **UDP** flows it keeps one real cellular UDP socket per flow (`sendto`/`recvfrom`); DNS on `:53` rides this path.
5. For **ICMP echo** it re-originates the ping over a cellular ICMP datagram socket.
6. Return packets are framed back to Windows the same way and written to Wintun.

### The one real trick — cellular egress
On the phone, every re-originated socket sets `IP_BOUND_IF` (`IPPROTO_IP`, value
25; IPv6 `IPV6_BOUND_IF`, 125) to the index of `pdp_ip0` **before** `connect()`,
so it egresses over cellular even when Wi-Fi is on. Interface discovery is
`if_nametoindex("pdp_ip0")`, then a `getifaddrs()` scan for any `pdp_ip*` (some
devices / dual-SIM name it differently). If no cellular interface is found we log
and fall back to the default route rather than failing.
*(A more robust future approach: do egress in Swift via `Network.framework`
`NWParameters.requiredInterfaceType = .cellular` and hand connected fds to Rust.
v1 is all-Rust with `pdp_ip0`.)*

### Transport (USB primary, Wi-Fi fallback)
The wire is a byte stream of `[u16 len][IP packet]` frames with two backends:
- **USB** via Apple's **usbmux**: the phone listens on `127.0.0.1:<port>`; on
  Windows we speak the usbmux plist protocol to `127.0.0.1:27015` natively in
  Rust (no `iproxy.exe`) — `ListDevices`, then `Connect{DeviceID, PortNumber}`;
  on success the same socket becomes a raw pipe to the phone's port.
- **Wi-Fi**: Windows connects directly to `<phone_ip>:<port>` on the LAN. No
  Apple service needed — handy for development. Selectable with `--transport`.

---

## Repository layout

| Path | What |
|------|------|
| `assets/icon.svg` | Source vector app icon (rasterized in CI). |
| `rickynet-wire/` | Shared Rust crate: `[u16 len][IP]` frame codec + constants. |
| `rickynet-core/` | Rust staticlib + C ABI (`rn_start`/`rn_stop`/`rn_stats`): ipstack netstack, per-flow cellular re-origination. Links into the iOS app. |
| `win/` | Rust binary `rickynet.exe`: Wintun, native usbmux + Wi-Fi transport, IP-Helper routing, elevation, egui GUI + `--headless` CLI. |
| `ios/` | SwiftUI app (RickyNet) + `project.yml` (XcodeGen). Links `librickynetcore.a`, embeds the app icon. |
| `.github/workflows/build.yml` | CI: unsigned `.ipa` (macOS) + `rickynet.exe` (Windows). |
| `docs/RUNBOOK.md` | Exact end-user setup. |

## Getting the artifacts
Push to GitHub (or open the **Actions** tab → **build**). Two artifacts are
produced on every run:
- **`RickyNet-unsigned-ipa`** → `RickyNet-unsigned.ipa` (install via SideStore/AltStore).
- **`RickyNet-windows`** → `rickynet.exe` + `wintun.dll` (run as Administrator).

Then follow **[docs/RUNBOOK.md](RUNBOOK.md)**.

## Building the FFI seam (what CI does)
```
cargo build -p rickynet-core --release --target aarch64-apple-ios
cbindgen --config rickynet-core/cbindgen.toml --crate rickynet-core \
         --output ios/Vendor/include/rickynetcore.h
# librickynetcore.a + the header are linked into the app by XcodeGen (project.yml).
```

---

## Honest limitations
- **Foreground-only iOS app.** iOS suspends background apps and kills the socket
  server, so the RickyNet screen must stay open and awake (the app sets
  `isIdleTimerDisabled`). Lock the phone or switch apps and the link drops. No
  background hacks in v1.
- **Free-provisioning = 7-day re-sign.** A free Apple ID sideload expires after
  ~7 days; SideStore/AltStore must re-sign the app periodically. (A paid
  developer account extends this to a year but isn't required.)
- **Throughput won't beat native tethering.** Every flow is terminated and
  re-originated in a userspace stack on a phone CPU; expect good-enough browsing,
  not line-rate. It is a convenience/consumption tool, not a performance one.
- **DPI can still fingerprint you.** Because traffic is genuinely re-originated
  from the phone's cellular stack there's **no TTL/OS tell** the way naive
  tethering has — but a carrier doing deep packet inspection can still infer
  desktop-shaped traffic patterns (TLS fingerprints, concurrent-connection fans,
  update pollers, etc.). RickyNet removes the obvious signal, not all of them.
- **Security note:** with "Accept over Wi-Fi (LAN)" enabled the phone listens on
  `0.0.0.0`, so anyone on the same network can reach the port. USB (loopback)
  is the private default.

## License
Dual-licensed under MIT or Apache-2.0.
