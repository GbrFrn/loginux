<p align="center">
  <img src="icons/loginux-128.png" width="96" height="96" alt="">
</p>

<h1 align="center">Loginux</h1>

<p align="center">Per-key RGB lighting for Logitech keyboards and mice on Linux.</p>

<p align="center">
  <a href="https://aur.archlinux.org/packages/loginux-git"><img alt="AUR package" src="https://img.shields.io/aur/version/loginux-git?label=AUR"></a>
  <a href="#licence"><img alt="Licence: MIT OR Apache-2.0" src="https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue"></a>
  <img alt="Platform: Linux" src="https://img.shields.io/badge/platform-Linux-informational">
  <img alt="Written in Rust" src="https://img.shields.io/badge/written%20in-Rust-orange">
</p>

![The Loginux editor painting a rainbow on a G316 X 98](docs/screenshots/editor.png)

Paint keys and light bars in a live editor, run the device's own effects, and
keep your profile through replugs and logins. Loginux writes only to the
device's RAM, so unplugging it always brings back the onboard lighting.

## Features

- A live editor: the keyboard follows while you paint.
- Built-in effects: Breathing, Colour Cycle, Wave and Ripple.
- A tray icon to switch profiles and open the editor (KDE Plasma; GNOME with
  the AppIndicator extension).
- Profiles are plain TOML files, and a background service keeps the chosen
  one applied.
- A new device is a data file, not code: see [CONTRIBUTING.md](CONTRIBUTING.md).

## Supported devices

| Device | Lighting |
| --- | --- |
| Logitech G316 X 98 | 95 keys and the 30-segment light bar |
| Logitech G903 HERO, on its cable | the logo ([why not the DPI lights](research/g903.md)) |

## Install

On Arch Linux, CachyOS and other Arch-based systems:

```sh
paru -S loginux-git                              # or: yay -S loginux-git
systemctl --user enable --now loginux.service    # keeps a profile applied, shows the tray icon
```

Elsewhere, build it with `cargo install --path .` and add the udev rule:
[install from source](docs/usage.md#install-from-source).

## Usage

Open **Loginux** from the application menu, or run `loginux`.

![Choosing a built-in effect in the editor](docs/screenshots/effects.png)

```sh
loginux use Wave     # keep a profile applied
loginux release      # hand the lighting back to the device
loginux status       # device, profile and settings
```

[docs/usage.md](docs/usage.md) covers the editor, the command line, the profile
format, effects, the service and how Loginux talks to a device.

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) explains how to add a device.
[research/](research/README.md) holds the protocol notes and device findings,
each labelled with how it was established.

## Licence

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or
the [MIT licence](LICENSE-MIT), at your option. The fonts in `fonts/`, and
`src/pixelfont.rs`, which is generated from Silkscreen, stay under the SIL Open
Font License 1.1. Unless you say otherwise, a contribution you submit for
inclusion is licensed as above, without any additional terms.
