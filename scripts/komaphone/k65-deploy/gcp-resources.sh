#!/usr/bin/env bash
# Cloud Shell: print the exact initial deployment by default. No account defaults change.
# Apply only after approval of docs/komaphone/k65-hosted-setup.md.
set -euo pipefail
project=komaphone
region=us-central1
zone=us-central1-a
bucket=komaphone-k65-backups-20261112
service_account=komaphone-backup@komaphone.iam.gserviceaccount.com
mode=${1:---plan}
if [[ "$mode" != --plan && "$mode" != --apply ]]; then
  printf '%s\n' 'Usage: bash gcp-resources.sh [--plan|--apply]'; exit 2
fi
run() {
  printf '%q ' gcloud --project="$project" --quiet "$@"; printf '\n'
  if [[ "$mode" == --apply ]]; then gcloud --project="$project" --quiet "$@"; fi
}
if [[ "$mode" == --apply ]]; then
  [[ ${KOMAPHONE_APPROVED_PROJECT:-} == "$project" ]] || {
    printf '%s\n' 'Apply requires KOMAPHONE_APPROVED_PROJECT=komaphone after reviewing the resource/cost plan.' >&2; exit 2;
  }
  command -v gcloud >/dev/null
  # Enabling these APIs does not launch resources. No IAM grants are made to the operator.
  run services enable compute.googleapis.com iam.googleapis.com iap.googleapis.com storage.googleapis.com
  # This is an initial provisioning script. Refuse to take over pre-existing resources.
  # A partial prior run is inventoried and resumed manually; never destroy data to retry it.
  for kind in instances disks addresses networks firewall-rules; do
    found=$(gcloud --project="$project" --quiet compute "$kind" list --filter='name~^komaphone-' --format='value(name)')
    [[ -z "$found" ]] || { printf 'Existing Komaphone %s: %s. Inspect before resuming.\n' "$kind" "$found" >&2; exit 1; }
  done
  found=$(gcloud --project="$project" --quiet iam service-accounts list --filter="email=$service_account" --format='value(email)')
  [[ -z "$found" ]] || { printf '%s\n' 'Backup service account already exists; inspect before resuming.' >&2; exit 1; }
fi
# Bucket creation is first: a globally occupied bucket name fails before VM/IP costs.
# The bucket is never adopted, made public or granted project-wide permissions.
run storage buckets create "gs://$bucket" --location="$region" --default-storage-class=STANDARD --uniform-bucket-level-access --public-access-prevention --soft-delete-duration=7d
run storage buckets update "gs://$bucket" --lifecycle-file="$(dirname -- "$0")/backup-lifecycle.json"
run iam service-accounts create komaphone-backup --display-name='Komaphone backup uploader'
run storage buckets add-iam-policy-binding "gs://$bucket" --member="serviceAccount:$service_account" --role=roles/storage.objectCreator
run compute networks create komaphone-private --subnet-mode=custom --bgp-routing-mode=regional
run compute networks subnets create komaphone-us-central1 --network=komaphone-private --region="$region" --range=10.42.0.0/24 --enable-private-ip-google-access
run compute firewall-rules create komaphone-iap-ssh --network=komaphone-private --direction=INGRESS --action=ALLOW --rules=tcp:22 --source-ranges=35.235.240.0/20 --target-tags=komaphone-sync
# Public access is a separate activation step after local hosted-auth probes pass.
run compute firewall-rules create komaphone-web --network=komaphone-private --direction=INGRESS --action=ALLOW --rules=tcp:80,tcp:443 --source-ranges=0.0.0.0/0 --target-tags=komaphone-sync --disabled
run compute disks create komaphone-data-1 --zone="$zone" --type=pd-standard --size=20GB --labels=app=komaphone,milestone=k65
run compute addresses create komaphone-ip --region="$region" --network-tier=PREMIUM
run compute instances create komaphone-sync-1 --zone="$zone" --machine-type=e2-small --provisioning-model=STANDARD --maintenance-policy=MIGRATE --image-family=debian-12 --image-project=debian-cloud --boot-disk-size=10GB --boot-disk-type=pd-standard --boot-disk-device-name=komaphone-boot --disk=name=komaphone-data-1,device-name=komaphone-data,mode=rw,boot=no,auto-delete=no --subnet=komaphone-us-central1 --address=komaphone-ip --network-tier=PREMIUM --service-account="$service_account" --scopes=https://www.googleapis.com/auth/cloud-platform --metadata=enable-oslogin=TRUE,block-project-ssh-keys=TRUE --tags=komaphone-sync --labels=app=komaphone,milestone=k65 --shielded-secure-boot --shielded-vtpm --shielded-integrity-monitoring --deletion-protection
run compute instances describe komaphone-sync-1 --zone="$zone" '--format=json(name,machineType,disks,networkInterfaces,serviceAccounts,metadata,tags,deletionProtection)'
run compute addresses describe komaphone-ip --region="$region" '--format=value(address)'
printf '%s\n' 'Initial resources only. Public firewall remains disabled. Do not enable it until hosted authentication and runtime readiness pass.'
