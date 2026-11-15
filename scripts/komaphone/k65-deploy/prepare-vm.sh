#!/usr/bin/env bash
# Run on the approved new Debian 12 VM, via IAP. No service is started here.
set -euo pipefail
umask 077
[[ ${1:-} == --initialize-empty-data-disk ]] || {
  printf '%s\n' 'Usage: sudo bash prepare-vm.sh --initialize-empty-data-disk' >&2; exit 2;
}
[[ $EUID == 0 ]] || { printf '%s\n' 'Run through sudo on the approved VM.' >&2; exit 2; }
device=/dev/disk/by-id/google-komaphone-data
mount_path=/var/lib/komaphone
[[ -b "$device" ]] || { printf '%s\n' 'Expected separately attached komaphone-data disk is absent.' >&2; exit 1; }
for tool in blkid wipefs lsblk findmnt mountpoint mkfs.ext4 gcloud caddy python3 curl; do command -v "$tool" >/dev/null; done
[[ $(lsblk -dn -o TYPE "$device") == disk ]] || { printf '%s\n' 'Expected a whole data disk.' >&2; exit 1; }
[[ $(lsblk -dn -o SIZE -b "$device") == 21474836480 ]] || { printf '%s\n' 'Expected the approved 20 GiB data disk.' >&2; exit 1; }
if mountpoint -q "$mount_path"; then
  [[ $(readlink -f -- "$(findmnt -n -o SOURCE --target "$mount_path")") == "$(readlink -f -- "$device")" ]] || {
    printf '%s\n' 'Mount belongs to a different device; refusing changes.' >&2; exit 1;
  }
else
  if [[ -e "$mount_path" ]] && [[ -n $(find "$mount_path" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    printf '%s\n' 'Mount directory contains files. Preserve them before mounting.' >&2; exit 1
  fi
  [[ -z $(lsblk -n -o MOUNTPOINTS "$device" | tr -d '[:space:]') ]] || {
    printf '%s\n' 'Data disk is already mounted elsewhere; refusing changes.' >&2; exit 1;
  }
  [[ $(lsblk -nr -o NAME "$device" | wc -l) == 1 ]] || {
    printf '%s\n' 'Data disk has partitions; refusing initialization.' >&2; exit 1;
  }
  signature=$(wipefs --no-act --noheadings --output TYPE "$device")
  if [[ -z "$signature" ]]; then
    mkfs.ext4 -m 0 "$device"
  elif [[ "$signature" != ext4 ]]; then
    printf '%s\n' 'Data disk already has an unexpected signature; refusing initialization.' >&2; exit 1
  fi
  disk_uuid=$(blkid -s UUID -o value "$device")
  [[ "$disk_uuid" =~ ^[0-9a-fA-F-]+$ ]] || { printf '%s\n' 'Missing disk UUID.' >&2; exit 1; }
  if grep -Eq '[[:space:]]/var/lib/komaphone[[:space:]]' /etc/fstab; then
    grep -Fqx "UUID=$disk_uuid /var/lib/komaphone ext4 defaults,nodev,nosuid 0 2" /etc/fstab || {
      printf '%s\n' 'Existing fstab mount differs; inspect before continuing.' >&2; exit 1;
    }
  else
    printf 'UUID=%s /var/lib/komaphone ext4 defaults,nodev,nosuid 0 2\n' "$disk_uuid" >> /etc/fstab
  fi
  mkdir -p "$mount_path"
  mount "$mount_path"
fi
if ! id -u komaphone >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/komaphone --shell /usr/sbin/nologin komaphone
fi
# Only the data root changes owner; never recursively reassign existing artwork.
chown komaphone:komaphone "$mount_path"
chmod 0700 "$mount_path"
install -d -o root -g root -m 0755 /opt/komaphone/releases /etc/komaphone
printf '%s\n' 'Data disk mounted and service user prepared. Service remains stopped.'
