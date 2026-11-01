#!/usr/bin/env bash
# Bound package-manager stalls before the native verification budget is consumed.
set -euo pipefail

if [[ $# != 1 ]]; then
  printf 'Expected one Ubuntu prerequisite profile.\n' >&2
  exit 2
fi
case "$1" in
  media) packages=(python3 nodejs unzip xz-utils) ;;
  glib) packages=(libglib2.0-dev pkg-config) ;;
  *) printf 'Unknown Ubuntu prerequisite profile.\n' >&2; exit 2 ;;
esac

# Kill the privileged process group at the deadline. A TERM/kill-after sequence
# can lose its final kill when the leader exits before an unresponsive child.
# A timed-out install fails this disposable hosted-runner job immediately.
apt=(sudo -n timeout --signal=KILL 180s
     env DEBIAN_FRONTEND=noninteractive apt-get
     -o Acquire::Retries=2
     -o Acquire::http::Timeout=20
     -o Acquire::https::Timeout=20
     -o DPkg::Lock::Timeout=30)

printf 'Updating Ubuntu package indexes (180-second deadline).\n'
"${apt[@]}" --error-on=any update
printf 'Installing Ubuntu prerequisites (180-second deadline).\n'
"${apt[@]}" install -y --no-install-recommends -- "${packages[@]}"
