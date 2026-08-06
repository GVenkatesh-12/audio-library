Drop your app logo here as `logo.png` and run
`./packaging/flatpak/build-flatpak.sh`.

The build script copies it into the flatpak bundle automatically (resized to
512x512 via ImageMagick if it is installed).

- File name must be exactly `logo.png`
- PNG transparent background is best (flatpak app icons are square)
- 512x512 px is the ideal source size

If you don't add a logo, the bundled `resources/icons/audio-library.png`
icon is used instead.