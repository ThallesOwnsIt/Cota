# Running Cota's live rounds on a server

The live rounds (`scripts/live/round.sh all`) run every 30 minutes on a small Linux server: today a VPS, later a
mini server at home. GitHub Actions stays as the backup. The server is disposable. Everything that matters lives on
the chain, in git (the ledger) and on Arweave, and the server only holds the keys. Moving to another machine is:
copy `/etc/cota`, run the installer, turn the timer on.

## What runs

- **`cota-round.timer`** (systemd) starts `cota-round.service` at :05 and :35 of every hour.
- **The service** runs `round.sh all` as the user `cota`. Each step runs in its own container
  (`deploy/compose.yml`):
  - `node-1`, `node-2`, `node-3`: each one sees only its own key;
  - `aggregate`: sees the operator key, the Arweave signer and the git deploy key.
- **The image** (`deploy/Dockerfile`) holds the Rust binaries, Python and Node.js. The checkout in `/srv/cota` is
  mounted at `/work`, so the ledger and the scripts stay on the host.
- **GitHub builds the image** (`.github/workflows/image.yml`) for amd64 and arm64 and publishes it at
  `ghcr.io/thallesownsit/cota`. The server only downloads it. The tag is a hash of the Rust sources and the
  Dockerfile (`deploy/image-tag.sh`), and the server computes the same hash from its checkout. So every round runs
  with the binaries of its own commit.
  - A machine with a good connection can build the image itself with `COTA_BUILD=1`, as an option.

| Path | What |
|---|---|
| `/srv/cota` | Checkout of the public repository (pulls over HTTPS, pushes over SSH with the deploy key) |
| `/etc/cota/cota.env` | Settings: RPC, ANA login, healthcheck URL, deploy hook (from `deploy/cota.env.example`) |
| `/etc/cota/secrets/` | `node-1.json`, `node-2.json`, `node-3.json`, `operator.json`, `archive.json`, `deploy_key` |

## Installing

1. A machine with Ubuntu 24.04 or Debian 12, amd64 or arm64, 2 GB of memory or more. Copy your SSH key to it.
2. As root: `git clone https://github.com/ThallesOwnsIt/Cota.git /srv/cota && bash /srv/cota/deploy/install.sh`.
   The installer sets up:
   - Docker, swap, SSH with keys only, the firewall and automatic security updates;
   - the user `cota` (uid 10001, the same as in the image) and `/etc/cota`;
   - a deploy key and the image of the current commit.
3. Add the printed deploy key to GitHub: repository → Settings → Deploy keys → *Allow write access*.
4. Copy the keys from your machine (they never go through git):
   `scp keys/node-{1,2,3}.json keys/operator.json keys/archive.json root@SERVER:/etc/cota/secrets/`
5. Fill `/etc/cota/cota.env` (ANA login, optionally a dedicated RPC, the healthcheck URL and the deploy hook).
6. Run the installer again to fix ownership. Test one round by hand:
   `systemctl start cota-round && journalctl -u cota-round -f`.
7. Turn the rounds on: `ENABLE=1 bash /srv/cota/deploy/install.sh`.

## A personal admin user instead of root

Once the server works, log in as your own user and use `sudo`, and turn off root's login. Every action is then
tied to a person, and root is no longer a way in.

1. As root, create the user, let it use sudo and give it your SSH key:
   ```sh
   adduser thalles                        # asks for a password: the one sudo will ask for
   usermod -aG sudo thalles
   install -d -m 700 -o thalles -g thalles /home/thalles/.ssh
   install -m 600 -o thalles -g thalles /root/.ssh/authorized_keys /home/thalles/.ssh/authorized_keys
   ```
2. **Keep the root session open.** In another terminal, check that the new user gets in and that sudo works:
   `ssh thalles@SERVER`, then `sudo whoami` (answers `root`).
3. Only then turn root's login off:
   ```sh
   sudo sed -i 's/^PermitRootLogin .*/PermitRootLogin no/' /etc/ssh/sshd_config.d/10-cota.conf
   sudo sshd -t && sudo systemctl reload ssh
   ```
   `deploy/install.sh` keeps it off when run again.

From then on, every root command in this guide runs with `sudo` (or after `sudo -i`). Keys are copied to your home
folder first and then moved into place:
`scp node-1.json thalles@SERVER:` followed by `sudo install -m 600 -o cota -g cota node-1.json /etc/cota/secrets/`.

## Day to day

- **Logs:** `journalctl -u cota-round --since today`.
- **Next rounds:** `systemctl list-timers cota-round.timer`.
- **Update after a push:** `bash /srv/cota/deploy/update.sh`. It pulls the code and the image of the new commit.
  - If the Rust code changed, the image takes about 15 minutes to be ready on GitHub. Until then the updater says
    so and the rounds keep the previous image.
- **Pause:** `systemctl disable --now cota-round.timer`. GitHub Actions takes over as the backup.
- **One step by hand:**

  ```sh
  cd /srv/cota && runuser -u cota -- docker compose --env-file /etc/cota/cota.env -f deploy/compose.yml \
    run --rm aggregate plan
  ```

  The `cd` matters when you are root. `docker compose` reads the current folder when it starts, and the user `cota`
  cannot read `/root`; without it, the command fails with "stat .: permission denied".

  Other steps: `node 1`, `anchor`, `publish`; with `-e PUSH=0` nothing is committed.

## Moving to another machine

1. On the old one: `systemctl disable --now cota-round.timer`.
2. Copy `/etc/cota` to the new one, keeping the permissions (`tar -C / -czf cota-etc.tgz etc/cota`).
3. On the new one: clone into `/srv/cota` and run the installer. It keeps the existing settings and keys and
   downloads the image for that architecture (amd64 or arm64).
4. Test a round, then `ENABLE=1 bash /srv/cota/deploy/install.sh`.
