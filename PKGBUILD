# Maintainer: Tom <deb@tombrk.de>
pkgname=eos-uvc
pkgver=$(grep '^version' "$startdir/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
pkgrel=1
pkgdesc="Canon EOS USB webcam streamer for Raspberry Pi 4"
arch=('arm64')
license=('MIT')
depends=('libgphoto2-6')
makedepends=('cargo' 'libgphoto2-dev' 'libclang-dev' 'pkg-config' 'libudev-dev')

prepare() {
    # Copy project source into srcdir (exclude makedeb/vcs artifacts)
    rsync -a --exclude=.jj --exclude=.git --exclude=target --exclude=pkg \
        "$startdir/" "$srcdir/eos-uvc/"
}

build() {
    cd "$srcdir/eos-uvc"
    export CARGO_TARGET_DIR="$srcdir/cargo-target"
    unset CFLAGS CXXFLAGS LDFLAGS
    cargo build --release
}

package() {
    install -Dm755 "$srcdir/cargo-target/release/eos-uvc" "$pkgdir/usr/bin/eos-uvc"
    install -Dm644 "$srcdir/eos-uvc/eos-uvc.service" "$pkgdir/usr/lib/systemd/system/eos-uvc.service"
}
