#!/usr/bin/env bash
# Approved dedicated Debian 12 VM only; install signed vendor packages, no credentials.
set -euo pipefail
[[ $EUID == 0 && $(hostname -s) == komaphone-sync-1 ]] || {
  printf '%s\n' 'Run as root only on the approved komaphone-sync-1 VM.' >&2; exit 2;
}
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y ca-certificates gnupg curl python3 util-linux debian-keyring debian-archive-keyring apt-transport-https
if ! command -v caddy >/dev/null; then
  curl --fail --silent --show-error --location https://dl.cloudsmith.io/public/caddy/stable/gpg.key |
    gpg --batch --yes --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl --fail --silent --show-error --location https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt \
    -o /etc/apt/sources.list.d/caddy-stable.list
  chmod 0644 /usr/share/keyrings/caddy-stable-archive-keyring.gpg /etc/apt/sources.list.d/caddy-stable.list
  apt-get update
  apt-get install -y caddy
fi
systemctl disable --now caddy
if ! command -v gcloud >/dev/null; then
  curl --fail --silent --show-error --location https://packages.cloud.google.com/apt/doc/apt-key.gpg |
    gpg --batch --yes --dearmor -o /usr/share/keyrings/cloud.google.gpg
  printf '%s\n' 'deb [signed-by=/usr/share/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main' \
    > /etc/apt/sources.list.d/google-cloud-sdk.list
  chmod 0644 /usr/share/keyrings/cloud.google.gpg /etc/apt/sources.list.d/google-cloud-sdk.list
  apt-get update
  apt-get install -y google-cloud-cli
fi
caddy version
gcloud --version
python3 --version
printf '%s\n' 'Signed packages installed; Caddy stopped and disabled pending private validation and approved public activation.'
