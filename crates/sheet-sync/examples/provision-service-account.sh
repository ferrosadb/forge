#!/usr/bin/env bash
# provision-service-account.sh — provisions a Google service account for
# headless `frg sheet pull/push` (see `crates/sheet-sync/src/service_account.rs`
# and `skills/sheet-sync/SKILL.md`).
#
# What this does:
#   1. Creates (or reuses) a Google Cloud project.
#   2. Enables the Google Sheets API on it.
#   3. Creates (or reuses) a service account in that project.
#   4. Mints a new JSON key for that service account and writes it to $KEY_OUT.
#
# The downloaded key JSON grants whoever holds it the ability to mint Sheets
# API access tokens *as this service account* — treat it as a bearer secret:
#
#   - `chmod 600` it (this script does that for you).
#   - NEVER commit it to git, paste it into a chat, or otherwise let it
#     leave your machine.
#   - A fresh service account has access to NOTHING by default — you must
#     separately share each target Google Sheet with its email address
#     (see the printed next-steps below).
#
# Usage:
#   ./provision-service-account.sh [KEY_OUT]
#
# Env vars (all optional):
#   PROJECT   Google Cloud project id to create or reuse.
#             Default: a freshly generated "sheet-sync-<random>" id.
#   SA_NAME   Service account id (the part before the @).
#             Default: "sheet-sync".
#   KEY_OUT   Where to write the downloaded key JSON.
#             Default: "$HOME/.forge/sheet-sync-sa.json".
#             May also be given as the first positional argument.
#
# Requires the `gcloud` CLI, already authenticated (`gcloud auth login`).

set -euo pipefail

KEY_OUT="${1:-${KEY_OUT:-"$HOME/.forge/sheet-sync-sa.json"}}"
SA_NAME="${SA_NAME:-sheet-sync}"

log() {
    echo "[provision-service-account] $*" >&2
}

fail() {
    echo "[provision-service-account] ERROR: $*" >&2
    exit 1
}

# --- Preflight -------------------------------------------------------------

command -v gcloud >/dev/null 2>&1 || fail \
    "gcloud CLI not found. Install the Google Cloud SDK: https://cloud.google.com/sdk/docs/install"

ACTIVE_ACCOUNT="$(gcloud auth list --filter=status:ACTIVE --format='value(account)' 2>/dev/null || true)"
if [ -z "$ACTIVE_ACCOUNT" ]; then
    fail "no active gcloud account. Run 'gcloud auth login' first, then re-run this script."
fi
log "using gcloud identity: $ACTIVE_ACCOUNT"

# --- Project -----------------------------------------------------------

if [ -z "${PROJECT:-}" ]; then
    # Generate a short random suffix so re-running without PROJECT set
    # doesn't collide with a prior run's project.
    PROJECT="sheet-sync-$(od -An -N2 -tu2 /dev/urandom | tr -d ' ')"
    log "no PROJECT set — creating a new project: $PROJECT"
    gcloud projects create "$PROJECT" --name="forge sheet-sync" -q
else
    log "PROJECT set — reusing/creating: $PROJECT"
    if ! gcloud projects describe "$PROJECT" >/dev/null 2>&1; then
        gcloud projects create "$PROJECT" --name="forge sheet-sync" -q || \
            log "project create reported an issue for $PROJECT — continuing, will verify it exists below"
    else
        log "project $PROJECT already exists — reusing it"
    fi
fi

gcloud projects describe "$PROJECT" >/dev/null 2>&1 || fail \
    "project $PROJECT does not exist and could not be created. Check gcloud output above."

gcloud config set project "$PROJECT" -q

log "enabling the Sheets API on $PROJECT"
gcloud services enable sheets.googleapis.com --project="$PROJECT" -q

# --- Service account -----------------------------------------------------

SA_EMAIL="${SA_NAME}@${PROJECT}.iam.gserviceaccount.com"

if gcloud iam service-accounts describe "$SA_EMAIL" --project="$PROJECT" >/dev/null 2>&1; then
    log "service account $SA_EMAIL already exists — reusing it"
else
    log "creating service account $SA_EMAIL"
    gcloud iam service-accounts create "$SA_NAME" \
        --project="$PROJECT" \
        --display-name="forge sheet sync" -q
fi

# --- Key -------------------------------------------------------------------

mkdir -p "$(dirname "$KEY_OUT")"

if [ -e "$KEY_OUT" ]; then
    fail "$KEY_OUT already exists — refusing to overwrite a possibly-live key. Remove it or pass a different KEY_OUT/first arg first."
fi

log "minting a new key for $SA_EMAIL -> $KEY_OUT"
gcloud iam service-accounts keys create "$KEY_OUT" --iam-account="$SA_EMAIL" -q
chmod 600 "$KEY_OUT"

# --- Loud final output -------------------------------------------------

cat >&2 <<EOF

============================================================
  Service account provisioned:  $SA_EMAIL
  Key written to (chmod 600):   $KEY_OUT
============================================================

NEXT STEPS:

  1. Share your Google Sheet with this service account as Editor:

         $SA_EMAIL

     (Open the sheet -> Share -> paste the address above -> Editor.)
     A service account has NO access to any sheet by default — this
     step is what actually grants it access.

  2. Point forge at the key, either:

         export FORGE_GOOGLE_SERVICE_ACCOUNT="$KEY_OUT"

     or add to .forge/config.toml:

         [google]
         service_account_path = "$KEY_OUT"

  3. Verify it works (no browser, no 'frg sheet auth' needed):

         frg sheet pull <alias> --dry-run

Keep "$KEY_OUT" private: never commit it, paste it into chat, or
share it outside this machine.
EOF
