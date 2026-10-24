# ACAM Android Accessory WinUSB driver (Windows)

`acam-accessory.inf` binds the in-box **WinUSB** driver to an Android phone once acam has switched it
into **Android Open Accessory (AOA)** mode, so `acam --usb` can open its bulk endpoints over USB
(libusb/rusb). This is the "proper" alternative to installing WinUSB with Zadig by hand.

acam's AOA *protocol* already works — it successfully switches the phone to accessory mode. The only
thing Windows needs is a WinUSB driver bound to the accessory **data interface**, which this INF does.

## Why this and not Zadig

- It targets **interface 0** (`MI_00`) of the accessory+ADB composite (PID `2D01`). That is the
  interface acam opens; Zadig's device list makes it easy to pick the wrong one (the ADB interface),
  which is why a manual Zadig attempt can "succeed" yet acam still fails with *Entity not found*.
- It publishes a `DeviceInterfaceGUID`, which libusb needs to enumerate a composite interface.
- It is a real driver package you can commit, re-use, and (for distribution) sign.

## One-time install

1. **Get the phone into accessory mode so the device exists to install onto.** With the phone plugged
   in and USB debugging on, run acam with the reset watchdog disabled so the accessory device stays
   enumerated while you install the driver:
   ```powershell
   $env:ACAM_USB_NO_RESET = 1
   ..\target\x86_64-pc-windows-msvc\debug\acam.exe --headless --usb
   ```
   Wait for `Phone switched to accessory mode`. It will then loop `failed to open the accessory …
   Entity not found` (expected — no driver yet). Leave this window running.

2. **Allow the unsigned driver to install** (winusb.sys itself is signed; only this INF package is
   not). On x64, reboot into *Disable driver signature enforcement*:
   Settings → System → Recovery → Advanced startup → **Restart now** → Troubleshoot → Advanced
   options → Startup Settings → Restart → press **7**. (This only affects installing; already-installed
   WinUSB keeps working after the next normal reboot.)

3. **Install the INF.** Either:
   - **Device Manager:** find the accessory device (under *Other devices* / *USB*, VID `18D1`,
     PID `2D00` or `2D01`) → right-click → *Update driver* → *Browse my computer* → point at this
     folder → install. Accept the "unverified publisher" warning.
   - **or pnputil (admin):**
     ```powershell
     pnputil /add-driver acam-accessory.inf /install
     ```

4. The device now shows as **ACAM Android Accessory** with the WinUSB driver, and the running acam
   should print `connected … USB` and start streaming. The binding persists, so future sessions just
   work — re-run `acam --headless --usb` (without the `ACAM_USB_NO_RESET` step).

## Notes

- **Accessory-only vs accessory+ADB.** With USB debugging **off**, the phone enters accessory-only
  mode (PID `2D00`, single interface) — the cleanest case. But libusb then cannot open the phone in
  its normal MTP mode to *send* the AOA switch, so in practice you keep USB debugging **on** (PID
  `2D01`) and this INF binds interface 0.
- **adb vs AOA.** Only one process may hold the USB device; don't run an `adb` server against the same
  phone while streaming.
- **Distribution.** For a shipped build, sign `acam-accessory.inf` + a generated `acam-accessory.cat`
  (EV certificate or Microsoft attestation signing) so step 2 isn't needed.
- **To revert:** Device Manager → the accessory device → *Uninstall device* (tick *Delete the driver
  software*), then unplug/replug.
