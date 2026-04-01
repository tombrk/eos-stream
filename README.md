# eos-stream

Turns a Raspberry Pi 4 into a USB webcam that streams from a Canon EOS camera.

Connect: **Canon EOS** → USB → **Pi 4** → USB-C → **Mac** (sees a webcam)

No capture card, no HDMI, no ffmpeg.

## Pi setup (one time)

```bash
sudo ./setup-pi.sh
sudo reboot
```

This enables the `dwc2` USB gadget overlay and loads `libcomposite`.

## Run

```bash
sudo ./eos-stream
```

The Pi registers as a UVC webcam over USB-C. Open FaceTime, OBS, etc. on the Mac — it shows up as "Canon EOS Webcam".

### Focus adjustment

Press <kbd>+</kbd> / <kbd>-</kbd> in the terminal to drive focus.
