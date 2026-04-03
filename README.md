# eos-stream

Turns a Raspberry Pi 4 into a USB webcam that streams from a Canon EOS camera.

Connect: **Canon EOS** → USB → **Pi 4** → USB-C → **Mac** (sees a webcam)

## Setup

1. `/boot/firmware/config.txt`:
  ```diff
   [all]
  +dtoverlay=dwc2
  ```
2. in `/etc/modules`:
  ```diff
  +dwc2
  +libcomposite
  ```
3. `systemctl enable --now eos-stream`
4. disable all unneccessary systemd services
5. Enable `overlayfs` in `raspi-config`

_Note_: To make changes later, use:
```
$ sudo mount -o remount,rw /media/root-ro
$ sudo chroot /media/root-ro dpkg -i /path/to/eos-stream.deb
```

### Focus adjustment

Press <kbd>+</kbd> / <kbd>-</kbd> in the terminal to drive focus.
