#!/usr/bin/env bash
# Prepares a Linux server (Ubuntu or Debian, a VPS or a mini server at home, amd64 or arm64) to run Cota's live
# rounds. Safe to run again at any time: each step only does what is missing. As root:
#
#   bash deploy/install.sh            (from a checkout)
#
# 1. Docker (official packages), git, firewall, automatic security updates
# 2. swap, when the machine has less than 4 GB of memory (the Rust build needs it)
# 3. SSH with keys only, if root already has an authorized key (nobody gets locked out)
# 4. user `cota`, the checkout in /srv/cota (pushes over SSH with a deploy key), /etc/cota with the settings and keys
# 5. the image of this commit (downloaded from GitHub, or built here with COTA_BUILD=1) and the systemd timer, which
#    is only turned on with ENABLE=1 and every key present
#
# Variables: DIR (default /srv/cota), REPO, PUSH_URL, SWAP_GB (default 4), COTA_BUILD (1 = build the image here),
#            ENABLE (1 = turn the timer on).
set -euo pipefail
DIR=${DIR:-/srv/cota}
REPO=${REPO:-https://github.com/ThallesOwnsIt/Cota.git}
PUSH_URL=${PUSH_URL:-git@github.com:ThallesOwnsIt/Cota.git}
ETC=/etc/cota
KEYS=(node-1.json node-2.json node-3.json operator.json archive.json deploy_key)
say() { printf '\n== %s\n' "$*"; }
[[ $EUID == 0 ]] || { echo "rode como root" >&2; exit 1; }

say "pacotes"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q git curl ufw unattended-upgrades ca-certificates
if ! command -v docker >/dev/null || ! docker compose version >/dev/null 2>&1; then
  # The distribution's own packages when it ships Compose v2 (Ubuntu 24.04+, from the provider's local mirror);
  # Docker's install script otherwise (Debian 12, Raspberry Pi OS).
  if apt-cache show docker-compose-v2 >/dev/null 2>&1; then
    apt-get install -y -q docker.io docker-compose-v2 docker-buildx
  else
    curl -fsSL https://get.docker.com | sh
  fi
fi
systemctl enable --now docker
cat > /etc/apt/apt.conf.d/20auto-upgrades <<'CONF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
CONF

say "memória"
mem_kb=$(awk '/MemTotal/ {print $2}' /proc/meminfo)
if (( mem_kb < 4 * 1024 * 1024 )) && [[ ! -f /swapfile-cota ]]; then
  fallocate -l "${SWAP_GB:-4}G" /swapfile-cota
  chmod 600 /swapfile-cota
  mkswap -q /swapfile-cota
  swapon /swapfile-cota
  echo '/swapfile-cota none swap sw 0 0' >> /etc/fstab
fi
free -h | sed -n '1,3p'

say "ssh"
CONF_SSH=/etc/ssh/sshd_config.d/10-cota.conf
# Root logs in with a key only; once a personal admin user exists and root login was turned off ("no", see
# docs/server.md), running the installer again keeps it off.
root_login=prohibit-password
grep -q '^PermitRootLogin no' "$CONF_SSH" 2>/dev/null && root_login=no
if [[ -s /root/.ssh/authorized_keys || $root_login == no ]]; then
  # sshd keeps the first value it reads, and sshd_config.d/ is read in order, before sshd_config: 10- wins over
  # the cloud image's 50-cloud-init.conf.
  cat > "$CONF_SSH" <<CONF
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin $root_login
CONF
  sshd -t
  systemctl reload ssh 2>/dev/null || systemctl reload sshd
  echo "login só com chave (root: $root_login)"
else
  echo "root não tem chave autorizada: login por senha mantido (copie a sua chave e rode de novo)"
fi

say "firewall"
ufw allow OpenSSH >/dev/null
ufw --force enable >/dev/null
ufw status | head -5

say "usuário e checkout"
# uid/gid 10001, the same as the user inside the image: the files the rounds write stay this user's.
ID=10001
getent group cota >/dev/null || groupadd -g "$ID" cota
id cota >/dev/null 2>&1 || useradd --system -u "$ID" -g cota --create-home --home-dir /home/cota --shell /bin/bash cota
if [[ $(id -u cota) != "$ID" || $(getent group cota | cut -d: -f3) != "$ID" ]]; then
  groupmod -g "$ID" cota
  usermod -u "$ID" -g "$ID" cota
  for d in /home/cota "$DIR" "$ETC"; do [[ -e $d ]] && chown -R cota:cota "$d"; done
fi
usermod -aG docker cota
# From / : the caller's folder (e.g. /root) may be unreadable for cota, and docker compose stats it.
as_cota() { (cd / && runuser -u cota -- "$@"); }
if [[ ! -d $DIR/.git ]]; then
  install -d -o cota -g cota "$DIR"
  as_cota git clone -q "$REPO" "$DIR"
fi
chown -R cota:cota "$DIR"
as_cota git -C "$DIR" remote set-url --push origin "$PUSH_URL"
as_cota git -C "$DIR" log --oneline -1

say "configuração e chaves ($ETC)"
install -d -o cota -g cota -m 700 "$ETC" "$ETC/secrets"
[[ -f $ETC/cota.env ]] || install -o cota -g cota -m 600 "$DIR/deploy/cota.env.example" "$ETC/cota.env"
# KEY=value in cota.env, replaced or added (files from an older example lack the newer keys).
set_env() {
  if grep -q "^$1=" "$ETC/cota.env"; then sed -i "s|^$1=.*|$1=$2|" "$ETC/cota.env"; else echo "$1=$2" >> "$ETC/cota.env"; fi
}
set_env COTA_UID "$(id -u cota)"
set_env COTA_GID "$(id -g cota)"
grep -q "^COTA_IMAGE=" "$ETC/cota.env" || set_env COTA_IMAGE ghcr.io/thallesownsit/cota
if [[ ! -f $ETC/secrets/deploy_key ]]; then
  as_cota ssh-keygen -q -t ed25519 -N '' -C "cota-server@$(hostname)" -f "$ETC/secrets/deploy_key"
fi
chown -R cota:cota "$ETC"
find "$ETC" -type f -exec chmod 600 {} +
missing=()
for k in "${KEYS[@]}"; do [[ -s $ETC/secrets/$k ]] || missing+=("$k"); done

say "imagem"
# The image of this checkout's commit: downloaded, ready, from GitHub (built by .github/workflows/image.yml), or
# built here with COTA_BUILD=1 (slow on a small machine; needs a good connection).
tag=$(as_cota "$DIR/deploy/image-tag.sh")
image=$(grep "^COTA_IMAGE=" "$ETC/cota.env" | cut -d= -f2)
set_env COTA_IMAGE_TAG "$tag"
if [[ ${COTA_BUILD:-0} == 1 ]]; then
  as_cota docker compose --env-file "$ETC/cota.env" -f "$DIR/deploy/compose.yml" build aggregate
else
  for n in 1 2 3 4 5; do
    if as_cota docker pull -q "$image:$tag"; then break; fi
    [[ $n == 5 ]] && { echo "não consegui baixar $image:$tag (a imagem desta versão já foi publicada pelo GitHub?)" >&2; exit 1; }
    sleep 30
  done
fi
echo "imagem: $image:$tag"

say "timer"
install -m 644 "$DIR/deploy/cota-round.service" "$DIR/deploy/cota-round.timer" /etc/systemd/system/
systemctl daemon-reload
if [[ ${#missing[@]} -gt 0 ]]; then
  echo "faltam chaves em $ETC/secrets: ${missing[*]}; o timer fica desligado"
elif [[ ${ENABLE:-0} == 1 ]]; then
  systemctl enable --now cota-round.timer
  systemctl list-timers cota-round.timer --no-pager
else
  echo "tudo pronto; para ligar as rodadas: ENABLE=1 bash $DIR/deploy/install.sh (ou systemctl enable --now cota-round.timer)"
fi

say "chave de deploy (GitHub → Settings → Deploy keys → Allow write access)"
cat "$ETC/secrets/deploy_key.pub"
