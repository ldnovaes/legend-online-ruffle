# Legend Online — Ruffle runtime fork

A fork of [Ruffle](https://github.com/ruffle-rs/ruffle) containing the compatibility and performance changes used by the [Legend Online Android client](https://github.com/ldnovaes/legend-online-android).

**Looking for the Android APK? [Download it from the Android releases](https://github.com/ldnovaes/legend-online-android/releases).** This repository contains the Flash runtime source; it does not provide an installable game client.

## Scope

The `legend-online` branch adds:

- Cancellation of in-flight `URLLoader` requests when `load()` or `close()` is called again.
- Optional on-disk caching of HTTP GET requests.
- Interning of short XML attribute values to reduce memory use.
- Avoiding unnecessary namespace allocation in `XML.name()`.
- Optional desktop frame snapshots used during development.

The Android repository pins a specific commit of this fork as a Git submodule. Follow its build guide to compile the APK with the correct runtime version.

## Platform relationship

Only the Android client uses this runtime. The separate [Linux PC client](https://github.com/ldnovaes/legend-online-linux) uses PySide6 and an Adobe Flash ActiveX host through Wine; it does not depend on Ruffle.

## Upstream

See [UPSTREAM_README.md](UPSTREAM_README.md) for the original Ruffle overview, build information, and contribution links. General Ruffle documentation is available at [ruffle.rs](https://ruffle.rs/).

This fork is maintained for the community Legend Online client and is not an official Ruffle release or a release from the game's publisher.

## License

Ruffle remains available under MIT or Apache-2.0. Original copyright and license notices are preserved in [LICENSE.md](LICENSE.md). Game assets and proprietary Flash binaries are not included.
