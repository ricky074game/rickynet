<p align="center">
  <img src="assets/icon.svg" width="128" alt="RickyNet">
</p>

<h1 align="center">RickyNet</h1>

<p align="center">
  Use an iPhone's <b>cellular data</b> as a Windows PC's internet connection, over the USB cable,
  <b>without</b> iOS Personal Hotspot — so it doesn't spend your tethering allotment.
</p>

<p align="center">
  <a href="docs/README.md">Architecture &amp; details</a> ·
  <a href="docs/RUNBOOK.md">Setup runbook</a> ·
  <a href="../../actions">Build artifacts (Actions)</a>
</p>

---

The desktop captures all its IP traffic on a Wintun TUN adapter and ships raw IP
packets over the cable to the phone; the **phone** runs a userspace TCP/IP stack
that terminates each flow and re-originates it as a **real cellular socket**
(`IP_BOUND_IF` → `pdp_ip0`). Full internet fidelity — real TCP, UDP, and ping —
not a TCP-only proxy. No Mac or paid Apple account required: the iOS app is an
unsigned `.ipa` you install with **SideStore/AltStore**, and everything builds in
**GitHub Actions**.

- **iOS app** (`ios/`) — SwiftUI shell linking the Rust core; no NetworkExtension, no special entitlement.
- **Rust core** (`rickynet-core/`) — ipstack netstack + per-flow cellular re-origination, C ABI (`rn_start`/`rn_stop`/`rn_stats`).
- **Windows client** (`win/`) — `rickynet.exe`: Wintun bridge, native usbmux + Wi-Fi transport, egui GUI (+ `--headless`).
- **Shared wire** (`rickynet-wire/`) — the `[u16 len][IP packet]` frame codec.

> See **[docs/README.md](docs/README.md)** for the full architecture and the honest
> limitations (foreground-only iOS app, 7-day free-provisioning re-sign,
> throughput, DPI), and **[docs/RUNBOOK.md](docs/RUNBOOK.md)** for exact setup.

Dual-licensed under MIT or Apache-2.0.
