#!/bin/bash
# One-time setup for Raspberry Pi 4 to enable USB gadget mode on the USB-C port.
# Run as root, then reboot.

set -euo pipefail

CONFIG="/boot/firmware/config.txt"
CMDLINE="/boot/firmware/cmdline.txt"

# Fallback for older Pi OS
[ -f "$CONFIG" ] || CONFIG="/boot/config.txt"
[ -f "$CMDLINE" ] || CMDLINE="/boot/cmdline.txt"

echo "Enabling dwc2 overlay..."
if ! grep -q "^dtoverlay=dwc2" "$CONFIG"; then
    echo "dtoverlay=dwc2" >> "$CONFIG"
    echo "  Added dtoverlay=dwc2 to $CONFIG"
else
    echo "  Already present in $CONFIG"
fi

echo "Loading dwc2 and libcomposite modules on boot..."
for mod in dwc2 libcomposite; do
    if ! grep -q "^$mod" /etc/modules; then
        echo "$mod" >> /etc/modules
        echo "  Added $mod to /etc/modules"
    else
        echo "  $mod already in /etc/modules"
    fi
done

echo ""
echo "Done. Reboot the Pi, then connect USB-C to your Mac and run eos-stream."
echo "The Mac should see a webcam called 'Canon EOS Webcam'."
