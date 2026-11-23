# Snap packaging

`snap/snapcraft.yaml` builds a strict-confinement Huddle snap with the GNOME
extension.

## Local build

```sh
sudo snap install snapcraft --classic
snapcraft pack --destructive-mode
sudo snap install ./huddle_*.snap --dangerous
```

## Publish to the Snap Store

1. Create an account at https://snapcraft.io/
2. Register the name (once):

```sh
snapcraft login
snapcraft register huddle
```

3. Export CI credentials:

```sh
snapcraft export-login snap-creds.txt
gh secret set SNAPCRAFT_STORE_CREDENTIALS --repo Cattens-Roses/Huddle < snap-creds.txt
rm snap-creds.txt
```

4. Push to `main` (publishes `beta` + `edge`) or tag `v*` (publishes all channels).

The GitHub Actions workflow is `.github/workflows/snap.yml`.
