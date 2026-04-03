# Maintainer: Tom <deb@tombrk.de>
pkgname=eos-stream
pkgver=0.2.6
pkgrel=1
pkgdesc="Canon EOS USB webcam streamer for Raspberry Pi 4"
arch=('arm64')
license=('MIT')
depends=('libgphoto2-6')
makedepends=('cargo' 'libgphoto2-dev' 'libclang-dev' 'pkg-config' 'libudev-dev')

prepare() {
    # Copy project source into srcdir (exclude makedeb/vcs artifacts)
    rsync -a --exclude=.jj --exclude=.git --exclude=target --exclude=pkg \
        "$startdir/" "$srcdir/eos-stream/"
}

build() {
    cd "$srcdir/eos-stream"
    export CARGO_TARGET_DIR="$srcdir/cargo-target"
    unset CFLAGS CXXFLAGS LDFLAGS
    cargo build --release
}

package() {
    install -Dm755 "$srcdir/cargo-target/release/eos-stream" "$pkgdir/usr/bin/eos-stream"
    install -Dm644 "$srcdir/eos-stream/eos-stream.service" "$pkgdir/usr/lib/systemd/system/eos-stream.service"
}
